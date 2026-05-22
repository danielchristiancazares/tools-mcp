use tokio_util::sync::CancellationToken;

tokio::task_local! {
    pub static CURRENT_CANCEL_TOKEN: CancellationToken;
}

pub fn current_cancellation_token() -> Option<CancellationToken> {
    CURRENT_CANCEL_TOKEN.try_with(Clone::clone).ok()
}
