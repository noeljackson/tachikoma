fn main() {
    connectrpc_build::Config::new()
        .files(&["proto/tachikoma/v1/tachikoma.proto"])
        .includes(&["proto"])
        .include_file("_connectrpc.rs")
        .compile()
        .expect("compile Connect RPC contracts");
}
