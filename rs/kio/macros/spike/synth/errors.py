"""Compile one deliberate mistake per case and save rustc's diagnostics.

usage: errors.py <cargo> <outdir>
"""

import os
import re
import subprocess
import sys

cargo, outdir = sys.argv[1], sys.argv[2]
os.makedirs(outdir, exist_ok=True)

PRELUDE = open("gen.py").read().split('PRELUDE = """', 1)[1].split('"""', 1)[0]

GOOD = """
		let a: u64 = wait!(A => self.src.poll_next(waiter))?;
		let label: String = format!("m-{a}");
		let b: u64 = wait!(B => self.src.poll_next(waiter))?;
		Poll::Ready(Ok(label.len() as u64 + a + b))
"""

CASES = {
    # A local used after a wait without a type.
    "untyped": GOOD.replace("let label: String =", "let label ="),
    # The annotation disagrees with the value.
    "wrong-annotation": GOOD.replace("let label: String =", "let label: u32 ="),
    # A type error inside a wait's poll expression.
    "poll-type": GOOD.replace("poll_next(waiter))?;\n\t\tlet label", "poll_next())?;\n\t\tlet label"),
    # A type error in run-once code.
    "segment-type": GOOD.replace('format!("m-{a}")', "a"),
    # A wait's poll moves a stored local (it only has `&mut`).
    "poll-moves": GOOD.replace(
        "wait!(B => self.src.poll_next(waiter))",
        "wait!(B => { drop(label); self.src.poll_next(waiter) })",
    ),
    # Forgot the `?`.
    "missing-try": GOOD.replace("let b: u64 = wait!(B => self.src.poll_next(waiter))?;", "let b: u64 = wait!(B => self.src.poll_next(waiter));"),
    # A hidden yield in run-once code.
    "hidden-yield": GOOD.replace(
        'let label: String = format!("m-{a}");',
        'let label: String = format!("m-{a}");\n\t\tlet c = ready!(self.src.poll_next(waiter))?;',
    ),
    # A wait inside a branch.
    "branch-wait": GOOD.replace(
        "let b: u64 = wait!(B => self.src.poll_next(waiter))?;",
        "let b: u64 = if a > 1 { wait!(B => self.src.poll_next(waiter))? } else { 0 };",
    ),
    # A borrow of the machine kept across a wait.
    "self-borrow": GOOD.replace('let label: String = format!("m-{a}");', "let label: &Source = &self.src;").replace(
        "label.len() as u64", "label.n"
    ),
    # A !Send local kept across a wait, then the machine is required Send.
    "not-send": GOOD.replace(
        'let label: String = format!("m-{a}");', 'let label: std::rc::Rc<String> = std::rc::Rc::new(format!("m-{a}"));'
    )
    + "SEND",
}

MACHINE = """
pub struct M {{
	src: Source,
	state: MState,
}}

#[kio::machine(self.state: MState)]
impl M {{
	pub fn new() -> Self {{
		Self {{ src: Source::default(), state: MState::Start }}
	}}

	pub fn poll(&mut self, waiter: &kio::Waiter) -> Poll<Result<u64, Error>> {{{body}	}}
}}
"""

ASYNC = """
pub async fn m(src: &mut Source) -> Result<u64, Error> {{{body}}}
"""

SEND_MACHINE = "\npub fn send(m: &M) {\n\tassert_send(m);\n}\n"
SEND_ASYNC = "\npub fn send(src: &mut Source) {\n\tassert_send(&m(src));\n}\n"


def to_async(body):
    """The same mistake written as an `async fn`, where it has one."""
    body = re.sub(r"wait!\(\w+ => \{ drop\(label\); self\.src\.poll_next\(waiter\) \}\)", "{ drop(label); kio::wait(|w| src.poll_next(w)).await }", body)
    body = re.sub(r"wait!\(\w+ => self\.src\.poll_next\(\)\)", "kio::wait(|w| src.poll_next()).await", body)
    body = re.sub(r"wait!\(\w+ => self\.src\.poll_next\(waiter\)\)", "kio::wait(|w| src.poll_next(w)).await", body)
    body = body.replace("ready!(self.src.poll_next(waiter))", "kio::wait(|w| src.poll_next(w)).await")
    body = body.replace("&self.src", "&*src").replace("let label: &Source", "let label: &Source")
    body = body.replace("Poll::Ready(Ok(", "Ok((").replace("))\n", "))\n")
    return body


for name, body in CASES.items():
    send = body.endswith("SEND")
    body = body.removesuffix("SEND")
    for style in ("macro", "async"):
        if style == "async" and name in ("untyped", "hidden-yield", "branch-wait"):
            continue
        src = PRELUDE
        if style == "macro":
            src += MACHINE.format(body=body) + (SEND_MACHINE if send else "")
        else:
            src += ASYNC.format(body=to_async(body)) + (SEND_ASYNC if send else "")
        open("src/lib.rs", "w").write(src)
        result = subprocess.run([cargo, "check", "-q", "--color=never"], capture_output=True, text=True)
        text = "\n".join(line for line in result.stderr.splitlines() if not line.startswith("mbx"))
        open(f"{outdir}/{name}.{style}.txt", "w").write(text)
        first = next((line for line in text.splitlines() if line.startswith("error")), "(compiled)")
        print(f"{name:18} {style:6} {first}")
