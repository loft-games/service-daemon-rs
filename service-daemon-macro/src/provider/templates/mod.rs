//! Template generators for built-in provider forms.
//!
//! This module contains generators for:
//! - Notify (Signal) template
//! - Broadcast Queue template
//! - Listen (TCP Listener) template
//! - UnixListen / UnixConnect templates
//! - NamedPipeListen / NamedPipeConnect templates
//! - LocalIpcListen / LocalIpcConnect templates
//!
//! Templates share common initialization logic via [`context::TemplateContext`].

mod context;
mod listen;
mod local_ipc;
mod memory;
mod named_pipe;
mod unix;

pub(super) use listen::generate_listen_template;
pub(super) use local_ipc::{
    generate_local_ipc_connect_template, generate_local_ipc_listen_template,
};
pub(super) use memory::{generate_broadcast_queue_template, generate_notify_template};
pub(super) use named_pipe::{
    generate_named_pipe_connect_template, generate_named_pipe_listen_template,
};
pub(super) use unix::{generate_unix_connect_template, generate_unix_listen_template};
