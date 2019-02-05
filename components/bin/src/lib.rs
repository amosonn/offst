#![feature(futures_api, async_await, await_macro, arbitrary_self_types)]
#![feature(nll)]
#![feature(try_from)]
#![feature(generators)]
#![feature(never_type)]

#[macro_use]
extern crate log;
extern crate clap;
#[macro_use]
extern crate serde_derive;

mod identity_file;
mod index_server_file;

pub use self::identity_file::{load_identity_from_file, store_identity_to_file};
