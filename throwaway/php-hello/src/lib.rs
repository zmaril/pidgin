use ext_php_rs::prelude::*;

/// Return a greeting built in Rust.
#[php_function]
pub fn pi_hello(name: String) -> String {
    format!("Hello, {name}, from Rust!")
}

/// Add two integers in Rust.
#[php_function]
pub fn pi_add(a: i64, b: i64) -> i64 {
    a + b
}

/// A trivial class with a method, proving class registration works.
#[php_class]
pub struct PiGreeter {
    prefix: String,
}

#[php_impl]
impl PiGreeter {
    pub fn __construct(prefix: String) -> Self {
        PiGreeter { prefix }
    }

    pub fn greet(&self, name: String) -> String {
        format!("{}: hello {} (from Rust)", self.prefix, name)
    }
}

#[php_module]
pub fn module(module: ModuleBuilder) -> ModuleBuilder {
    module
}
