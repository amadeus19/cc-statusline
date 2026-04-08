//! Tokens component implementation
//!
//! Displays token usage information with cached transcript statistics and adaptive progress bars.

use async_trait::async_trait;

use super::base::{Component, ComponentFactory, ComponentOutput, RenderContext};
use super::progress_bar::{self, ProgressBarParams};
use crate::config::{BaseComponentConfig, Config, TokensComponentConfig};
use crate::storage;
use crate::utils::model_parser::parse_model_id;

#[derive(Clone, Debug)]
struct TokenUsageInfo {
    used: u64,
    total: u64,
}

/// Tokens component
pub struct TokensComponent {
    config: TokensComponentConfig,
}

impl TokensComponent {
    #[must_use]
    pub const fn new(config: TokensComponentConfig) -> Self {
        Self { config }
    }

    async fn fetch_usage_from_cache(&self, ctx: &RenderContext) -> Option<TokenUsageInfo> {
        if let Some(mock_tokens) = ctx
            .input
            .extra
            .get("__mock__")
            .and_then(|mock| mock.get("tokensUsage"))
        {
            let used = mock_tokens
                .get("context_used")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            if used == 0 && !self.config.show_zero {
                return None;
            }
            let window = mock_tokens
                .get("context_window")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or_else(|| self.context_window_for_model(ctx));
            return Some(TokenUsageInfo {
                used,
                total: window,
            });
        }

        if let Some(session_id) = ctx.input.session_id.as_deref() {
            if let Ok(Some(tokens)) = storage::get_session_tokens(session_id).await {
                let used = tokens.context_used;
                if used == 0 && !self.config.show_zero {
                    return None;
                }
                let window = self.context_window_for_model(ctx);
                return Some(TokenUsageInfo {
                    used,
                    total: window,
                });
            }
        }
        if self.config.show_zero {
            let window = self.context_window_for_model(ctx);
            return Some(TokenUsageInfo {
                used: 0,
                total: window,
            });
        }
        None
    }

    fn context_window_for_model(&self, ctx: &RenderContext) -> u64 {
        let default_window = self
            .config
            .context_windows
            .get("default")
            .copied()
            .unwrap_or(200_000);

        let Some(model) = ctx.input.model.as_ref() else {
            return default_window;
        };

        if let Some(id) = model.id.as_ref() {
            // Priority 1: Exact match from config
            if let Some(value) = self.config.context_windows.get(id) {
                return *value;
            }

            // Priority 2: Infer from model ID params (e.g., [1m])
            if let Some(parsed) = parse_model_id(id) {
                if let Some(window) = parsed.infer_context_window() {
                    return window;
                }
            }
        }

        default_window
    }

    fn build_progress_bar(&self, ctx: &RenderContext, percentage: f64) -> Option<String> {
        if !self.config.show_progress_bar {
            return None;
        }

        let width = self.config.progress_width.max(1) as usize;
        let gradient_enabled = self.config.show_gradient
            || matches!(ctx.config.theme.as_str(), "powerline" | "capsule");
        let supports_colors = ctx.terminal.supports_colors();

        let filled_char = self
            .config
            .progress_bar_chars
            .filled
            .chars()
            .next()
            .unwrap_or('█');
        let empty_char = self
            .config
            .progress_bar_chars
            .empty
            .chars()
            .next()
            .unwrap_or('░');
        let backup_char = self
            .config
            .progress_bar_chars
            .backup
            .chars()
            .next()
            .unwrap_or('▓');

        let params = ProgressBarParams {
            percentage,
            width,
            filled_char,
            empty_char,
            backup_char,
            backup_threshold: self.config.thresholds.backup,
            gradient_enabled,
            supports_colors,
            baseline_percentage: None,
        };

        progress_bar::build_progress_bar(&params)
    }

    fn select_status_icon(&self, ctx: &RenderContext, percentage: f64) -> Option<String> {
        let thresholds = &self.config.thresholds;
        let status = if percentage >= thresholds.critical {
            TokenStatusKind::Critical
        } else if percentage >= thresholds.backup {
            TokenStatusKind::Backup
        } else {
            return None;
        };

        let icons = &self.config.status_icons;
        let terminal_cfg = &ctx.config.terminal;
        let terminal = &ctx.terminal;
        let style = &ctx.config.style;

        if terminal_cfg.force_text {
            return icon_for_kind(&icons.text, status).map(std::string::ToString::to_string);
        }
        if terminal_cfg.force_nerd_font {
            if let Some(icon) = icon_for_kind(&icons.nerd, status) {
                return Some(icon.to_string());
            }
        }
        if terminal_cfg.force_emoji {
            if let Some(icon) = icon_for_kind(&icons.emoji, status) {
                return Some(icon.to_string());
            }
        }

        if terminal.supports_nerd_font
            && style
                .enable_nerd_font
                .is_enabled(terminal.supports_nerd_font)
        {
            if let Some(icon) = icon_for_kind(&icons.nerd, status) {
                return Some(icon.to_string());
            }
        }

        if terminal.supports_emoji && style.enable_emoji.is_enabled(terminal.supports_emoji) {
            if let Some(icon) = icon_for_kind(&icons.emoji, status) {
                return Some(icon.to_string());
            }
        }

        icon_for_kind(&icons.text, status).map(std::string::ToString::to_string)
    }

    fn select_color(&self, percentage: f64) -> String {
        let thresholds = &self.config.thresholds;

        if percentage >= thresholds.danger {
            self.config.colors.danger.clone()
        } else if percentage >= thresholds.warning {
            self.config.colors.warning.clone()
        } else {
            self.config.colors.safe.clone()
        }
    }

    fn format_usage(&self, used: u64, overhead: u64, total: u64) -> String {
        if self.config.show_raw_numbers {
            if overhead > 0 {
                format!("({used}(+{overhead})/{total})")
            } else {
                format!("({used}/{total})")
            }
        } else {
            let used_k = progress_bar::to_f64(used) / 1_000.0;
            let total_k = progress_bar::to_f64(total) / 1_000.0;
            if overhead > 0 {
                let overhead_k = progress_bar::to_f64(overhead) / 1_000.0;
                format!("({used_k:.1}(+{overhead_k:.1})k/{total_k:.0}k)")
            } else {
                format!("({used_k:.1}k/{total_k:.0}k)")
            }
        }
    }
}

#[async_trait]
impl Component for TokensComponent {
    fn name(&self) -> &'static str {
        "tokens"
    }

    fn is_enabled(&self, _ctx: &RenderContext) -> bool {
        self.config.base.enabled
    }

    async fn render(&self, ctx: &RenderContext) -> ComponentOutput {
        if !self.is_enabled(ctx) {
            return ComponentOutput::hidden();
        }

        let Some(usage) = self.fetch_usage_from_cache(ctx).await else {
            return ComponentOutput::hidden();
        };

        let total = usage.total.max(1);

        // system_overhead: 用户手动配置的系统提示词固定占用
        let overhead = self.config.system_overhead.unwrap_or(0);
        let effective_used = usage.used + overhead;
        let percentage =
            (progress_bar::to_f64(effective_used) / progress_bar::to_f64(total)) * 100.0;
        let clamped_percentage = percentage.clamp(0.0, 999.9);

        let mut parts = Vec::new();

        if let Some(bar) = self.build_progress_bar(ctx, clamped_percentage) {
            parts.push(format!("[{bar}]"));
        }

        if self.config.show_percentage {
            parts.push(format!("{clamped_percentage:.1}%"));
        }

        parts.push(self.format_usage(usage.used, overhead, total));

        if let Some(status_icon) = self.select_status_icon(ctx, clamped_percentage) {
            parts.push(status_icon);
        }

        let text = parts.join(" ");
        let color = self.select_color(clamped_percentage);
        let icon = self.select_icon(ctx);

        ComponentOutput::new(text)
            .with_icon(icon.unwrap_or_default())
            .with_icon_color(color.clone())
            .with_text_color(color)
    }

    fn base_config(&self, _ctx: &RenderContext) -> Option<&BaseComponentConfig> {
        Some(&self.config.base)
    }
}

fn icon_for_kind(set: &crate::config::TokenIconSetConfig, kind: TokenStatusKind) -> Option<&str> {
    match kind {
        TokenStatusKind::Backup => (!set.backup.is_empty()).then_some(set.backup.as_str()),
        TokenStatusKind::Critical => (!set.critical.is_empty()).then_some(set.critical.as_str()),
    }
}

#[derive(Clone, Copy)]
enum TokenStatusKind {
    Backup,
    Critical,
}

/// Factory for creating Tokens components
pub struct TokensComponentFactory;

impl ComponentFactory for TokensComponentFactory {
    fn create(&self, config: &Config) -> Box<dyn Component> {
        Box::new(TokensComponent::new(config.components.tokens.clone()))
    }

    fn name(&self) -> &'static str {
        "tokens"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{ColorSupport, TerminalCapabilities};
    use crate::config::AutoDetect;
    use crate::core::InputData;
    use serde_json::json;
    use std::sync::Arc;

    #[allow(clippy::field_reassign_with_default)]
    fn build_tokens_config(
        configure: impl FnOnce(&mut TokensComponentConfig),
    ) -> TokensComponentConfig {
        let mut config = TokensComponentConfig::default();
        configure(&mut config);
        config
    }

    #[allow(clippy::field_reassign_with_default)]
    fn build_input(configure: impl FnOnce(&mut InputData)) -> InputData {
        let mut input = InputData::default();
        configure(&mut input);
        input
    }

    fn create_test_context_with_tokens(tokens: i64) -> RenderContext {
        let used = u64::try_from(tokens).unwrap_or(0);

        let input = build_input(|input| {
            input.session_id = Some("mock-session".to_string());
            input.extra = json!({
                "__mock__": {
                    "tokensUsage": {
                        "context_used": used
                    }
                }
            });
        });

        RenderContext {
            input: Arc::new(input),
            config: Arc::new(Config::default()),
            terminal: TerminalCapabilities::default(),
        }
    }

    #[tokio::test]
    async fn test_tokens_contains_percentage() {
        let component = TokensComponent::new(TokensComponentConfig::default());
        let ctx = create_test_context_with_tokens(1_000);

        let output = component.render(&ctx).await;
        assert!(output.visible);
        assert!(output.text.contains('%'));
    }

    #[tokio::test]
    async fn test_tokens_raw_numbers_format() {
        let config = build_tokens_config(|config| {
            config.show_percentage = false;
            config.show_progress_bar = false;
            config.show_raw_numbers = true;
        });

        let component = TokensComponent::new(config);
        let ctx = create_test_context_with_tokens(1_500);

        let output = component.render(&ctx).await;
        assert!(output.visible);
        assert!(output.text.contains("(1500/200000)"));
    }

    #[tokio::test]
    async fn test_tokens_progress_bar_enabled() {
        let config = build_tokens_config(|config| {
            config.show_progress_bar = true;
            config.show_percentage = false;
            config.show_raw_numbers = false;
        });

        let component = TokensComponent::new(config);
        let ctx = create_test_context_with_tokens(50_000);

        let output = component.render(&ctx).await;
        assert!(output.visible);
        assert!(output.text.contains('['));
    }

    #[tokio::test]
    async fn test_tokens_progress_bar_gradient() {
        let config = build_tokens_config(|config| {
            config.show_progress_bar = true;
            config.show_percentage = false;
            config.show_raw_numbers = false;
            config.show_gradient = true;
            config.progress_width = 6;
        });

        let component = TokensComponent::new(config);
        let mut ctx = create_test_context_with_tokens(100_000);
        let config = Arc::make_mut(&mut ctx.config);
        config.theme = "classic".to_string();
        config.style.enable_colors = AutoDetect::Bool(true);
        let mut terminal = ctx.terminal.clone();
        terminal.color_support = ColorSupport::TrueColor;
        let ctx = RenderContext { terminal, ..ctx };

        let output = component.render(&ctx).await;
        assert!(output.visible);
        assert!(output.text.contains("\x1b[38;2"));
    }

    #[tokio::test]
    async fn test_tokens_zero_hidden() {
        let config = build_tokens_config(|config| {
            config.show_zero = false;
        });

        let component = TokensComponent::new(config);
        let ctx = create_test_context_with_tokens(0);

        let output = component.render(&ctx).await;
        assert!(!output.visible);
    }

    #[tokio::test]
    async fn test_tokens_zero_shown() {
        let config = build_tokens_config(|config| {
            config.show_zero = true;
        });

        let component = TokensComponent::new(config);
        let ctx = create_test_context_with_tokens(0);

        let output = component.render(&ctx).await;
        assert!(output.visible);
    }

    #[tokio::test]
    async fn test_tokens_disabled() {
        let config = build_tokens_config(|config| {
            config.base.enabled = false;
        });

        let component = TokensComponent::new(config);
        let ctx = create_test_context_with_tokens(1000);

        let output = component.render(&ctx).await;
        assert!(!output.visible);
    }

    #[tokio::test]
    async fn test_tokens_mock_context_window_override() {
        let input = build_input(|input| {
            input.session_id = Some("mock-session".to_string());
            input.extra = json!({
                "__mock__": {
                    "tokensUsage": {
                        "context_used": 20u64,
                        "context_window": 100u64
                    }
                }
            });
        });

        let ctx = RenderContext {
            input: Arc::new(input),
            config: Arc::new(Config::default()),
            terminal: TerminalCapabilities::default(),
        };

        let config = build_tokens_config(|config| {
            config.show_progress_bar = false;
            config.show_percentage = false;
            config.show_raw_numbers = true;
        });

        let component = TokensComponent::new(config);
        let output = component.render(&ctx).await;

        assert!(output.visible);
        assert!(output.text.contains("(20/100)"));
    }

    // ==================== 上下文窗口智能推断测试 ====================

    #[tokio::test]
    async fn test_context_window_infer_1m_model() {
        use crate::core::ModelInfo;

        let input = build_input(|input| {
            input.session_id = Some("mock-session".to_string());
            input.model = Some(ModelInfo {
                id: Some("claude-sonnet-4-5-20250929[1m]".to_string()),
                display_name: None,
            });
            input.extra = json!({
                "__mock__": {
                    "tokensUsage": {
                        "context_used": 100_000u64
                    }
                }
            });
        });

        let ctx = RenderContext {
            input: Arc::new(input),
            config: Arc::new(Config::default()),
            terminal: TerminalCapabilities::default(),
        };

        let config = build_tokens_config(|config| {
            config.show_progress_bar = false;
            config.show_percentage = false;
            config.show_raw_numbers = true;
        });

        let component = TokensComponent::new(config);
        let output = component.render(&ctx).await;

        assert!(output.visible);
        // Should infer 1M context window from [1m] suffix
        assert!(output.text.contains("(100000/1000000)"));
    }

    #[tokio::test]
    async fn test_context_window_exact_match_takes_priority() {
        use crate::core::ModelInfo;

        let input = build_input(|input| {
            input.session_id = Some("mock-session".to_string());
            input.model = Some(ModelInfo {
                id: Some("claude-sonnet-4-5-20250929[1m]".to_string()),
                display_name: None,
            });
            input.extra = json!({
                "__mock__": {
                    "tokensUsage": {
                        "context_used": 50_000u64
                    }
                }
            });
        });

        let ctx = RenderContext {
            input: Arc::new(input),
            config: Arc::new(Config::default()),
            terminal: TerminalCapabilities::default(),
        };

        let config = build_tokens_config(|config| {
            config.show_progress_bar = false;
            config.show_percentage = false;
            config.show_raw_numbers = true;
            // Exact match should take priority over inference
            config
                .context_windows
                .insert("claude-sonnet-4-5-20250929[1m]".to_string(), 500_000);
        });

        let component = TokensComponent::new(config);
        let output = component.render(&ctx).await;

        assert!(output.visible);
        // Should use exact match (500k) instead of inferred (1M)
        assert!(output.text.contains("(50000/500000)"));
    }

    #[tokio::test]
    async fn test_context_window_fallback_to_default() {
        use crate::core::ModelInfo;

        let input = build_input(|input| {
            input.session_id = Some("mock-session".to_string());
            input.model = Some(ModelInfo {
                id: Some("claude-opus-4-1-20250805".to_string()), // No [1m] suffix
                display_name: None,
            });
            input.extra = json!({
                "__mock__": {
                    "tokensUsage": {
                        "context_used": 10_000u64
                    }
                }
            });
        });

        let ctx = RenderContext {
            input: Arc::new(input),
            config: Arc::new(Config::default()),
            terminal: TerminalCapabilities::default(),
        };

        let config = build_tokens_config(|config| {
            config.show_progress_bar = false;
            config.show_percentage = false;
            config.show_raw_numbers = true;
        });

        let component = TokensComponent::new(config);
        let output = component.render(&ctx).await;

        assert!(output.visible);
        // Should fallback to default 200k
        assert!(output.text.contains("(10000/200000)"));
    }
}
