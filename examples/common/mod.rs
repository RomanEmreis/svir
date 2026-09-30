//! Where the examples find a model server.
//!
//! svir reads no environment variables on its own. The examples do, so that they run against any
//! server as they are:
//!
//! - `SVIR_URL`: the server, `http://127.0.0.1:1234` unless set
//! - `SVIR_MODEL`: the model; `cargo run --example models` lists what the server has

#![allow(dead_code)]

/// The server to talk to.
pub fn url() -> String {
    std::env::var("SVIR_URL").unwrap_or_else(|_| "http://127.0.0.1:1234".to_owned())
}

/// The model to talk to.
pub fn model() -> String {
    std::env::var("SVIR_MODEL").unwrap_or_else(|_| {
        eprintln!("set SVIR_MODEL to a model the server has; list them with:");
        eprintln!("    cargo run --example models");
        std::process::exit(2)
    })
}
