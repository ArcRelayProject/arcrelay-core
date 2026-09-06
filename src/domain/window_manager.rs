use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceInfo {
    /// Unique space ID (CGS space id on macOS, virtual desktop id on Windows)
    pub space_id: u64,
    /// Human-readable label ("Desktop 1", "Desktop 2", …).
    pub label: String,
    /// Whether this is the currently active space
    pub is_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowInfo {
    /// OS window id
    pub window_id: u32,
    /// Window title
    pub title: String,
    /// Owner application name
    pub app_name: String,
    /// Whether this window is currently focused
    pub is_focused: bool,
    /// PNG-encoded thumbnail (scaled down)
    pub thumbnail_png: Vec<u8>,
    /// Which space this window belongs to (0 = unknown / all-spaces)
    pub space_id: u64,
}

#[derive(Debug, Clone)]
pub struct WindowStateSample {
    pub windows: Vec<WindowInfo>,
    pub spaces: Vec<SpaceInfo>,
    pub on_screen_window_ids: Vec<u32>,
    pub focused_app_name: Option<String>,
}

#[async_trait::async_trait]
pub trait WindowManagerRepository: Send + Sync {
    /// Subscribe to platform window/focus/Space notifications. Backends that
    /// cannot expose notifications return `None` and are sampled as fallback.
    fn subscribe_changes(&self) -> Option<tokio::sync::broadcast::Receiver<()>> {
        None
    }

    /// Synchronous frontmost-application lookup for native callbacks such as
    /// clipboard watchers that do not run inside a Tokio runtime.
    fn focused_app_name_now(&self) -> crate::error::Result<String>;

    /// List visible windows (with optional thumbnails)
    async fn list_windows(&self) -> crate::error::Result<Vec<WindowInfo>>;

    /// Capture thumbnails only for the requested windows. Implementations
    /// should avoid capturing unrelated windows.
    async fn list_windows_with_thumbnails(
        &self,
        window_ids: &[u32],
    ) -> crate::error::Result<Vec<WindowInfo>> {
        let requested = window_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>();
        let mut windows = self.list_windows().await?;
        for window in &mut windows {
            if !requested.contains(&window.window_id) {
                window.thumbnail_png.clear();
            }
        }
        Ok(windows)
    }

    /// Fast window enumeration **without** capturing thumbnails.
    /// Used by the push/subscription path so focus changes can be
    /// broadcast with minimal overhead.
    async fn list_windows_meta(&self) -> crate::error::Result<Vec<WindowInfo>>;

    /// Get the name of the currently focused (frontmost) application
    async fn focused_app_name(&self) -> crate::error::Result<String>;

    /// Focus (bring to front) a specific window by its window ID.
    /// If the window lives on a different Space the implementation
    /// should switch to that Space first.
    async fn focus_window(&self, window_id: u32) -> crate::error::Result<()>;

    /// List available virtual desktops / Spaces.
    async fn list_spaces(&self) -> crate::error::Result<Vec<SpaceInfo>>;

    /// Switch to a specific Space by its ID.
    async fn switch_space(&self, space_id: u64) -> crate::error::Result<()>;

    /// Return sorted window IDs visible on the current Space only.
    /// Used for lightweight space-change detection (the set changes
    /// when the active Space changes).
    async fn on_screen_window_ids(&self) -> crate::error::Result<Vec<u32>>;

    /// Take one coherent metadata sample for subscription/change detection.
    async fn sample_window_state(&self) -> crate::error::Result<WindowStateSample> {
        let windows = self.list_windows_meta().await?;
        let focused_app_name = windows
            .iter()
            .find(|window| window.is_focused)
            .map(|window| window.app_name.clone());
        let on_screen_window_ids = self.on_screen_window_ids().await.unwrap_or_default();
        let spaces = self.list_spaces().await.unwrap_or_default();
        Ok(WindowStateSample {
            windows,
            spaces,
            on_screen_window_ids,
            focused_app_name,
        })
    }

    /// Take a coherent state sample while capturing only selected thumbnails.
    async fn sample_window_state_with_thumbnails(
        &self,
        window_ids: &[u32],
    ) -> crate::error::Result<WindowStateSample> {
        let mut sample = self.sample_window_state().await?;
        let thumbnails = self.list_windows_with_thumbnails(window_ids).await?;
        let thumbnails = thumbnails
            .into_iter()
            .filter(|window| !window.thumbnail_png.is_empty())
            .map(|window| (window.window_id, window.thumbnail_png))
            .collect::<std::collections::HashMap<_, _>>();
        for window in &mut sample.windows {
            if let Some(thumbnail) = thumbnails.get(&window.window_id) {
                window.thumbnail_png = thumbnail.clone();
            }
        }
        Ok(sample)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeWindowManager {
        auxiliary_queries_fail: bool,
    }

    fn windows(with_thumbnails: bool) -> Vec<WindowInfo> {
        vec![
            WindowInfo {
                window_id: 1,
                title: "Editor".into(),
                app_name: "IDE".into(),
                is_focused: true,
                thumbnail_png: if with_thumbnails { vec![1] } else { Vec::new() },
                space_id: 10,
            },
            WindowInfo {
                window_id: 2,
                title: "Browser".into(),
                app_name: "Browser".into(),
                is_focused: false,
                thumbnail_png: if with_thumbnails { vec![2] } else { Vec::new() },
                space_id: 10,
            },
        ]
    }

    #[async_trait::async_trait]
    impl WindowManagerRepository for FakeWindowManager {
        fn focused_app_name_now(&self) -> crate::error::Result<String> {
            Ok("IDE".into())
        }

        async fn list_windows(&self) -> crate::error::Result<Vec<WindowInfo>> {
            Ok(windows(true))
        }

        async fn list_windows_meta(&self) -> crate::error::Result<Vec<WindowInfo>> {
            Ok(windows(false))
        }

        async fn focused_app_name(&self) -> crate::error::Result<String> {
            Ok("IDE".into())
        }

        async fn focus_window(&self, _window_id: u32) -> crate::error::Result<()> {
            Ok(())
        }

        async fn list_spaces(&self) -> crate::error::Result<Vec<SpaceInfo>> {
            if self.auxiliary_queries_fail {
                Err(crate::error::Error::Other("spaces unavailable".into()))
            } else {
                Ok(vec![SpaceInfo {
                    space_id: 10,
                    label: "Desktop".into(),
                    is_active: true,
                }])
            }
        }

        async fn switch_space(&self, _space_id: u64) -> crate::error::Result<()> {
            Ok(())
        }

        async fn on_screen_window_ids(&self) -> crate::error::Result<Vec<u32>> {
            if self.auxiliary_queries_fail {
                Err(crate::error::Error::Other("ids unavailable".into()))
            } else {
                Ok(vec![1, 2])
            }
        }
    }

    #[tokio::test]
    async fn selective_thumbnail_listing_clears_unrequested_images() {
        let manager = FakeWindowManager {
            auxiliary_queries_fail: false,
        };
        let result = manager.list_windows_with_thumbnails(&[2]).await.unwrap();

        assert!(result[0].thumbnail_png.is_empty());
        assert_eq!(result[1].thumbnail_png, vec![2]);
    }

    #[tokio::test]
    async fn state_sample_derives_focus_and_tolerates_auxiliary_failures() {
        let manager = FakeWindowManager {
            auxiliary_queries_fail: true,
        };
        let sample = manager.sample_window_state().await.unwrap();

        assert_eq!(sample.focused_app_name.as_deref(), Some("IDE"));
        assert!(sample.on_screen_window_ids.is_empty());
        assert!(sample.spaces.is_empty());
    }

    #[tokio::test]
    async fn state_sample_merges_only_requested_thumbnails() {
        let manager = FakeWindowManager {
            auxiliary_queries_fail: false,
        };
        let sample = manager
            .sample_window_state_with_thumbnails(&[1])
            .await
            .unwrap();

        assert_eq!(sample.windows[0].thumbnail_png, vec![1]);
        assert!(sample.windows[1].thumbnail_png.is_empty());
        assert_eq!(sample.on_screen_window_ids, vec![1, 2]);
        assert_eq!(sample.spaces.len(), 1);
    }
}
