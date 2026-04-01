use entanglement_sys::*;

#[derive(Debug, thiserror::Error)]
pub enum EntError {
    #[error("socket error")]
    Socket,
    #[error("not connected")]
    NotConnected,
    #[error("connection denied")]
    ConnectionDenied,
    #[error("connection timeout")]
    ConnectionTimeout,
    #[error("backpressured")]
    Backpressured,
    #[error("payload too large")]
    PayloadTooLarge,
    #[error("invalid argument")]
    InvalidArgument,
    #[error("channel in use")]
    ChannelInUse,
    #[error("channel not found")]
    ChannelNotFound,
    #[error("channel slots full")]
    ChannelSlotsFull,
    #[error("channel rejected")]
    ChannelRejected,
    #[error("channel timeout")]
    ChannelTimeout,
    #[error("pool full")]
    PoolFull,
    #[error("duplicate packet")]
    DuplicatePacket,
    #[error("already started")]
    AlreadyStarted,
    #[error("unknown error code: {0}")]
    Unknown(i32),
}

impl From<i32> for EntError {
    fn from(code: i32) -> Self {
        match code {
            x if x == ent_error_ENT_ERROR_SOCKET as i32 => EntError::Socket,
            x if x == ent_error_ENT_ERROR_NOT_CONNECTED as i32 => EntError::NotConnected,
            x if x == ent_error_ENT_ERROR_CONNECTION_DENIED as i32 => EntError::ConnectionDenied,
            x if x == ent_error_ENT_ERROR_CONNECTION_TIMEOUT as i32 => EntError::ConnectionTimeout,
            x if x == ent_error_ENT_ERROR_BACKPRESSURED as i32 => EntError::Backpressured,
            x if x == ent_error_ENT_ERROR_PAYLOAD_TOO_LARGE as i32 => EntError::PayloadTooLarge,
            x if x == ent_error_ENT_ERROR_INVALID_ARGUMENT as i32 => EntError::InvalidArgument,
            x if x == ent_error_ENT_ERROR_CHANNEL_IN_USE as i32 => EntError::ChannelInUse,
            x if x == ent_error_ENT_ERROR_CHANNEL_NOT_FOUND as i32 => EntError::ChannelNotFound,
            x if x == ent_error_ENT_ERROR_CHANNEL_SLOTS_FULL as i32 => EntError::ChannelSlotsFull,
            x if x == ent_error_ENT_ERROR_CHANNEL_REJECTED as i32 => EntError::ChannelRejected,
            x if x == ent_error_ENT_ERROR_CHANNEL_TIMEOUT as i32 => EntError::ChannelTimeout,
            x if x == ent_error_ENT_ERROR_POOL_FULL as i32 => EntError::PoolFull,
            x if x == ent_error_ENT_ERROR_DUPLICATE_PACKET as i32 => EntError::DuplicatePacket,
            x if x == ent_error_ENT_ERROR_ALREADY_STARTED as i32 => EntError::AlreadyStarted,
            _ => EntError::Unknown(code),
        }
    }
}

pub type EntResult<T> = Result<T, EntError>;

/// Convert a C API return value to a Result.
/// Values >= 0 are success (often bytes sent), negative values are errors.
pub fn check_err(code: i32) -> EntResult<i32> {
    if code >= 0 {
        Ok(code)
    } else {
        Err(EntError::from(code))
    }
}
