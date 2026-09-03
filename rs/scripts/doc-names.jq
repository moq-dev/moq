# Find workspace targets that would render to the same `target/doc/<dir>`.
#
# `cargo doc` writes each documented target to a directory named after the target with
# dashes folded to underscores, so two targets that differ only there collide: both
# rustdoc processes race to clean it, and the warning escalates to a hard error
# depending on which wins.
#
# Reads `cargo metadata --no-deps` and prints one line per colliding group.
[ .packages[]
  | . as $pkg
  # Cargo skips a binary that would land on the same directory as a library in its own
  # package, documenting only the library, so such a pair never actually collides.
  # Compared after folding, matching cargo: a `foo_bar` lib beside a `foo-bar` bin is
  # suppressed too (verified against cargo 1.95). Only cross-package pairs collide.
  | [ $pkg.targets[]
      | select([.kind[]] | any(. == "lib" or . == "rlib" or . == "dylib"
          or . == "cdylib" or . == "staticlib" or . == "proc-macro"))
      | (.name | gsub("-"; "_"))
    ] as $libs
  | $pkg.targets[]
  # `doc = false` in the manifest, which is how a reported pair gets resolved.
  | select(.doc)
  | select([.kind[]] | any(. == "lib" or . == "rlib" or . == "dylib"
      or . == "cdylib" or . == "staticlib" or . == "proc-macro" or . == "bin"))
  | select((([.kind[]] | any(. == "bin")) and ((.name | gsub("-"; "_")) | IN($libs[]))) | not)
  | { dir: (.name | gsub("-"; "_")), package: $pkg.name, target: .name, kind: .kind[0] }
]
| group_by(.dir)
| map(select(length > 1))
| .[]
| "  target/doc/\(.[0].dir) <- " + (map("\(.package) [\(.kind)] \(.target)") | join(" and "))
