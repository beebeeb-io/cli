//! The upload driver against a scripted API and a storage server of its own, through the real
//! `stream_encrypt_upload`: the wire the server really speaks (bodies as in server #331), what the
//! storage server sees, and what the driver does after each failure.

mod support;
