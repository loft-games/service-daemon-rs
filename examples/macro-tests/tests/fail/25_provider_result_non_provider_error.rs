use service_daemon::provider;

#[derive(Clone, Default)]
pub struct OrdinaryResultValue;

#[derive(Clone, Debug)]
pub struct OtherError;

#[provider]
async fn ordinary_result_provider() -> Result<OrdinaryResultValue, OtherError> {
    Ok(OrdinaryResultValue)
}

fn main() {}
