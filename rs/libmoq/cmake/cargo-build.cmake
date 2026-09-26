# Run with `cmake -DCARGO_BUILD_FLAG=... -DHEADER=... -P cargo-build.cmake` from rs/libmoq.
#
# Builds libmoq, then copies moq.h to HEADER. build.rs writes the header into
# its OUT_DIR, whose hashed path only cargo's JSON messages name.
execute_process(
    COMMAND cargo build --locked ${CARGO_BUILD_FLAG} --message-format=json-render-diagnostics
    OUTPUT_VARIABLE _messages
    COMMAND_ERROR_IS_FATAL ANY
)

string(REGEX MATCH "{\"reason\":\"build-script-executed\",\"package_id\":\"[^\"]*/libmoq#[^\n]*" _message "${_messages}")
if(NOT _message)
    message(FATAL_ERROR "cargo reported no build script output for libmoq")
endif()
string(JSON _out_dir GET "${_message}" out_dir)

file(COPY_FILE "${_out_dir}/include/moq.h" "${HEADER}" ONLY_IF_DIFFERENT)
