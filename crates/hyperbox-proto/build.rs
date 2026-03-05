fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("protoc path");
    // Build scripts run in a single-threaded process here, so setting PROTOC is safe.
    unsafe { std::env::set_var("PROTOC", protoc) };

    tonic_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["../../proto/hyperbox/v1/control.proto"], &["../../proto"])
        .expect("compile protobufs");
}
