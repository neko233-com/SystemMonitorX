#![cfg(windows)]

pub mod export;
pub mod gpu;
pub mod hardware;
pub mod monitor;
pub mod network_process;
pub mod performance;
pub mod platform;
pub mod process_details;
pub mod sessions;
pub mod updater;

pub mod runtime;

pub mod process_tree;

pub mod directx;

pub mod update_install;

pub mod file_signature;

pub mod cpu_topology;

pub mod process_groups;

pub mod name_order;

pub mod network_ports;

rust_i18n::i18n!("locales", fallback = "en-US");

pub mod i18n;
