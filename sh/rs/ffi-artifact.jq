# Select only this invocation's cdylib, rejecting an implicit different target.
def path: gsub("\\\\"; "/");
($root | path) as $base |
[.[] | select(.reason == "compiler-artifact" and .target.name == "moq_ffi") |
 .filenames[] | path | select(endswith("/" + $library))] | unique |
if length != 1 then error("expected one moq-ffi cdylib from Cargo") else .[0] end |
. as $file | ($file | rtrimstr("/" + $library)) as $directory |
if $directory == ($base + "/" + $target + "/" + $profile) or
   ($target == $host and $directory == ($base + "/" + $profile))
then $file else error("Cargo artifact does not match the requested target/profile: " + $file) end
