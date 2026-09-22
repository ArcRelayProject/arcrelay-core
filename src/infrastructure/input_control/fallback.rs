use super::*;

pub struct NativeInputControl;

impl NativeInputControl {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait::async_trait]
impl InputControlRepository for NativeInputControl {
    fn permission_state(&self) -> InputPermissionState {
        InputPermissionState::Unsupported
    }
    fn open_permission_settings(&self) -> Result<()> {
        Err(Error::NotSupported("remote input".into()))
    }
    fn validate_events(&self, _events: &[InputEvent]) -> Result<()> {
        Err(Error::NotSupported("remote input".into()))
    }
    async fn apply_events(&self, _events: &[InputEvent]) -> Result<()> {
        Err(Error::NotSupported("remote input".into()))
    }
    async fn paste_clipboard(&self, _is_text: bool) -> Result<()> {
        Err(Error::NotSupported("clipboard paste".into()))
    }
    async fn type_text_as_keys(&self, _text: &str) -> Result<()> {
        Err(Error::NotSupported("simulated keyboard input".into()))
    }
    async fn release_all(&self) -> Result<()> {
        Ok(())
    }
}
