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
  # Cargo skips a binary whose name matches a library in its own package, documenting
  # only the library, so such a pair never actually collides. Names are compared as
  # spelled: a `foo_bar` lib beside a `foo-bar` bin is not suppressed by cargo and does
  # collide, so it has to stay reportable.
  | [ $pkg.targets[]
      | select([.kind[]] | any(. == "lib" or . == "rlib" or . == "dylib"
          or . == "cdylib" or . == "staticlib" or . == "proc-macro"))
      | .name
    ] as $libs
  | $pkg.targets[]
  # `doc = false` in the manifest, which is how a reported pair gets resolved.
  | select(.doc)
  | select([.kind[]] | any(. == "lib" or . == "rlib" or . == "dylib"
      or . == "cdylib" or . == "staticlib" or . == "proc-macro" or . == "bin"))
  | select((([.kind[]] | any(. == "bin")) and (.name | IN($libs[]))) | not)
  | { dir: (.name | gsub("-"; "_")), package: $pkg.name, target: .name, kind: .kind[0] }
]
| group_by(.dir)
| map(select(length > 1))
| .[]
| "  target/doc/\(.[0].dir) <- " + (map("\(.package) [\(.kind)] \(.target)") | join(" and "))
