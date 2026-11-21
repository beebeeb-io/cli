//! Which chunk format the pipeline writes for an unrecorded pin. Core's default is v2 (a v2 session never resumes),
//! so the resume tests, which need a session a new encryptor may continue, write v1 by default; a test opts into
//! core's real default with [`CoreDefault::on`].

use beebeeb_core::chunk_stream::{ChunkEncryptor, ChunkFormat, ExpectedChunkFormat};

#[cfg(not(test))]
pub(super) fn apply(enc: ChunkEncryptor, _pin: ExpectedChunkFormat) -> Result<ChunkEncryptor, beebeeb_core::CoreError> {
    Ok(enc)
}

#[cfg(test)]
thread_local!(static CORE_DEFAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

/// Test guard: the pipeline writes what core writes by default (v2) while it lives, on this thread.
#[cfg(test)]
pub(crate) struct CoreDefault;

#[cfg(test)]
impl CoreDefault {
    pub(crate) fn on() -> Self {
        CORE_DEFAULT.with(|c| c.set(true));
        Self
    }
}

#[cfg(test)]
impl Drop for CoreDefault {
    fn drop(&mut self) {
        CORE_DEFAULT.with(|c| c.set(false));
    }
}

#[cfg(test)]
pub(super) fn apply(enc: ChunkEncryptor, pin: ExpectedChunkFormat) -> Result<ChunkEncryptor, beebeeb_core::CoreError> {
    if pin == ExpectedChunkFormat::Unrecorded && !CORE_DEFAULT.with(|c| c.get()) {
        enc.with_format(ChunkFormat::V1Legacy)
    } else {
        Ok(enc)
    }
}

/// The format `apply` makes the encryptor write for `pin`.
pub(super) fn format_for(pin: ExpectedChunkFormat) -> ChunkFormat {
    #[cfg(test)]
    if pin == ExpectedChunkFormat::Unrecorded && !CORE_DEFAULT.with(|c| c.get()) {
        return ChunkFormat::V1Legacy;
    }
    beebeeb_core::chunk_stream::chunk_format_for_pin(pin)
}
