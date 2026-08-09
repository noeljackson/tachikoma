//! Tachikoma's durable domain model and generated Connect RPC contract.

pub mod adapter;
pub mod ai;
pub mod analysis;
pub mod config;
pub mod investigation;
pub mod kubernetes;
pub mod opensnitch;
pub mod rpc;
pub mod store;
pub mod tui;
pub mod web;

pub mod proto {
    connectrpc::include_generated!();
}
