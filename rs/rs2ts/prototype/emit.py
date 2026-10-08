"""A fail-closed scalar LLBC experiment, not the production translator."""

import json
import re
import sys


def untag(value):
    if isinstance(value, dict) and set(value) == {"Untagged"}:
        return untag(value["Untagged"])
    return value


def variant(value):
    value = untag(value)
    if isinstance(value, str):
        return value, None
    if isinstance(value, dict) and len(value) == 1:
        return next(iter(value.items()))
    raise ValueError(f"expected an enum variant, got {value!r}")


def scalar(value):
    kind, inner = variant(value)
    if kind != "Scalar":
        raise ValueError(f"unsupported type: {kind}")
    kind, inner = variant(inner)
    if kind == "Bool":
        return "Bool"
    if kind == "Integer":
        _, integer = variant(inner)
        if integer in ["U8", "U16", "U32", "I8", "I16", "I32", "U64", "I64"]:
            return integer
    raise ValueError(f"unsupported scalar: {value!r}")


def ts_type(ty):
    kind = scalar(ty)
    return "boolean" if kind == "Bool" else "U64" if kind in ["U64", "I64"] else "number"


class Emitter:
    def __init__(self):
        self.helpers = set()

    def helper(self, name, *args):
        self.helpers.add(name)
        return f"{name}({', '.join(args)})"

    def place(self, value):
        kind, inner = variant(value["kind"])
        if kind == "Local":
            return f"v{inner}"
        raise ValueError(f"unsupported place: {kind}")

    def operand(self, value):
        kind, inner = variant(value)
        if kind in ["Copy", "Move"]:
            # The accepted scalar values are immutable, so neither operation aliases mutation.
            return self.place(inner), scalar(inner["ty"])
        if kind == "Const":
            const, ty = untag(inner)
            kind, data = variant(const)
            if kind == "Bool":
                return str(data).lower(), "Bool"
            if kind == "Integer":
                _, (_, raw) = variant(data)
                integer = int(raw)
                ty = scalar(ty)
                width = int(ty[1:])
                minimum = 0 if ty.startswith("U") else -(1 << (width - 1))
                maximum = (1 << width) - 1 if ty.startswith("U") else (1 << (width - 1)) - 1
                if not minimum <= integer <= maximum:
                    raise ValueError(f"constant outside {ty}: {integer}")
                if ty in ["U64", "I64"]:
                    bits = integer % (1 << 64)
                    return f"new U64({bits >> 32}, {bits & 0xffffffff})", ty
                return str(integer), ty
        raise ValueError(f"unsupported operand: {kind}")

    def rvalue(self, value):
        kind, data = variant(value)
        if kind == "Use":
            return self.operand(data[0])[0]
        if kind == "BinaryOp":
            op, left, right = data
            op, mode = variant(op)
            left, ty = self.operand(left)
            right, _ = self.operand(right)
            if ty in ["U64", "I64"]:
                if op in ["BitXor", "BitAnd"]:
                    return self.helper("xor" if op == "BitXor" else "and", left, right)
                if op in ["Shl", "Shr"] and mode == "Wrap":
                    name = "shl" if op == "Shl" else "sar" if ty == "I64" else "shr"
                    return self.helper(name, left, right)
                if op == "Eq":
                    return f"{left}.equals({right})"
            elif op in ["Lt", "Eq"]:
                return f"{left} {'<' if op == 'Lt' else '==='} {right}"
            raise ValueError(f"unsupported binary operation: {op}({mode}) on {ty}")
        if kind == "UnaryOp":
            op, value = data
            op, mode = variant(op)
            value, ty = self.operand(value)
            if op == "Neg" and mode == "Wrap" and ty == "I64":
                return self.helper("neg", value)
            if op == "Cast":
                cast_kind, types = variant(mode)
                if cast_kind == "Scalar":
                    source, target = [scalar({"Scalar": t}) for t in types]
                    if source == target or {source, target} == {"I64", "U64"}:
                        return value
                    if source == "I32" and target == "U32":
                        return f"{value} >>> 0"
            raise ValueError(f"unsupported unary operation: {op} on {ty}")
        raise ValueError(f"unsupported rvalue: {kind}")

    def function(self, decl):
        meta = decl["item_meta"]
        if meta["has_errors"] or decl["signature"]["is_unsafe"]:
            raise ValueError("refusing an unsafe or partially extracted function")
        if any(decl["generics"].values()):
            raise ValueError("generic functions are outside this scalar prototype")
        parts = [part["Ident"][0] for part in meta["name"]]
        name = parts[-1]
        if not re.fullmatch(r"[a-zA-Z_][a-zA-Z_0-9]*", name):
            raise ValueError(f"unsupported function name: {name}")
        kind, body = variant(decl["body"])
        if kind != "Structured":
            raise ValueError(f"unsupported body: {kind}")
        locals_ = body["locals"]["locals"]
        count = body["locals"]["arg_count"]
        params = [f"v{i}: {ts_type(locals_[i]['ty'])}" for i in range(1, count + 1)]
        lines = [f"/** Translated from {'::'.join(parts)}; i64 values use two's complement U64 bits. */",
                 f"export function {name}({', '.join(params)}): {ts_type(decl['signature']['output'])} {{"]
        for local in locals_:
            ts_type(local["ty"])
        assigned = {f"v{i}" for i in range(1, count + 1)}
        for statement in body["body"]["statements"]:
            try:
                kind, data = variant(statement["kind"])
                if kind in ["StorageLive", "StorageDead", "Nop"]:
                    continue
                if kind == "Assign":
                    target = self.place(data[0])
                    if target in assigned:
                        raise ValueError("reassignment is outside this straight-line prototype")
                    assigned.add(target)
                    lines.append(f"\tconst {target}: {ts_type(data[0]['ty'])} = {self.rvalue(data[1])};")
                elif kind == "Assert":
                    cleanup = data["on_unwind"]["statements"]
                    if any(s["kind"] not in ["UnwindResume", "UnwindTerminate", "UndefinedBehavior", "Nop"] for s in cleanup):
                        raise ValueError("nontrivial unwind cleanup is outside the scalar prototype")
                    if set(data["on_failure"]) != {"Panic"}:
                        raise ValueError("unsupported assertion failure")
                    cond, _ = self.operand(data["assert"]["cond"])
                    lines.append(f"\tif ({cond} !== {str(data['assert']['expected']).lower()}) throw new Error(\"Rust assertion failed\");")
                elif kind == "Return":
                    lines.append("\treturn v0;")
                else:
                    raise ValueError(f"unsupported statement: {kind}")
            except ValueError as error:
                location = untag(statement["span"])["data"]["beg"]
                raise ValueError(f"{'::'.join(parts)}:{location['line']}:{location['col']}: {error}") from error
        return "\n".join(lines + ["}"])


def emit(data):
    if data["charon_version"] != "0.1.284":
        raise ValueError("expected Charon 0.1.284")
    if data["has_errors"]:
        raise ValueError("refusing partial Charon output")
    emitter = Emitter()
    roots = [f for f in data["translated"]["fun_decls"] if f and f["item_meta"]["started_from"]]
    if not roots:
        raise ValueError("no extraction roots")
    functions = []
    for decl in roots:
        try:
            functions.append(emitter.function(decl))
        except ValueError as error:
            name = "::".join(part["Ident"][0] for part in decl["item_meta"]["name"])
            if str(error).startswith(name + ":"):
                raise
            location = untag(decl["item_meta"]["span"])["data"]["beg"]
            raise ValueError(f"{name}:{location['line']}:{location['col']}: {error}") from error
    names = [f["item_meta"]["name"][-1]["Ident"][0] for f in roots]
    if len(set(names)) != len(names):
        raise ValueError("function names collide after removing the module path")
    if set(names) & (emitter.helpers | {"U64"}):
        raise ValueError("function names collide with runtime imports")
    return "\n".join([
        "// Generated by the scalar LLBC prototype. Do not edit.",
        'import { U64 } from "../../../js/net/src/util/u64";',
        f'import {{ {", ".join(sorted(emitter.helpers))} }} from "./runtime";',
        "", "\n\n".join(functions), "",
    ])


if __name__ == "__main__":
    try:
        with open(sys.argv[1]) as source:
            output = emit(json.load(source))
        sys.stdout.write(output)
    except (ValueError, KeyError, TypeError, IndexError) as error:
        sys.exit(f"rs2ts prototype: {error}")
