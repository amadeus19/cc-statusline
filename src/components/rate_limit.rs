//! Rate limit component
//!
//! Displays GLM platform quota usage (5h token + MCP) as an independent
//! second line in the statusline.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use super::base::{Component, ComponentFactory, ComponentOutput, RenderContext};
use super::progress_bar::{self, ProgressBarParams};
use crate::api::{GlmApiClient, UsageStats};
use crate::config::{Config, RateLimitComponentConfig};

/// Cache entry for in-memory caching
#[derive(Clone)]
struct CacheEntry {
    stats: UsageStats,
    timestamp: Instant,
}

/// Rate limit component displaying GLM quota usage
pub struct RateLimitComponent {
    config: RateLimitComponentConfig,
    cache: Arc<Mutex<Option<CacheEntry>>>,
}

impl RateLimitComponent {
    #[must_use]
    pub fn new(config: RateLimitComponentConfig) -> Self {
        Self {
            config,
            cache: Arc::new(Mutex::new(None)),
        }
    }

    /// Get usage stats through the three-level cache chain:
    /// Memory → File → API call; on API failure → File → stale Memory
    fn get_usage_stats(&self) -> Option<UsageStats> {
        if !self.config.glm_usage.enabled {
            return None;
        }

        let ttl = Duration::from_secs(self.config.glm_usage.cache_ttl);

        // Level 1: Check memory cache
        if let Some(entry) = self
            .cache
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().cloned())
        {
            if entry.timestamp.elapsed() < ttl {
                return Some(entry.stats);
            }
        }

        // Level 2: Try API call
        if let Ok(client) = GlmApiClient::from_env() {
            let stats = client.fetch_usage_stats().ok();

            if let Some(ref s) = stats {
                // Update memory cache
                if let Ok(mut guard) = self.cache.lock() {
                    *guard = Some(CacheEntry {
                        stats: s.clone(),
                        timestamp: Instant::now(),
                    });
                }

                // Update file cache (best-effort)
                let cached = crate::storage::rate_limit_cache::CachedRateLimit::new(s.clone());
                if let Err(err) = crate::storage::rate_limit_cache::write_cache(&cached) {
                    eprintln!("[statusline] rate limit cache write failed: {err}");
                }

                Some(s.clone())
            } else {
                // API failed, try file cache
                if let Some(cached) = crate::storage::rate_limit_cache::read_cache() {
                    // Update memory cache with file data
                    if let Ok(mut guard) = self.cache.lock() {
                        *guard = Some(CacheEntry {
                            stats: cached.stats.clone(),
                            timestamp: Instant::now(),
                        });
                    }
                    return Some(cached.stats);
                }

                // Return stale memory cache if available
                self.cache
                    .lock()
                    .ok()
                    .and_then(|guard| guard.as_ref().map(|e| e.stats.clone()))
            }
        } else {
            // Can't create client (missing env vars), try file cache
            if let Some(cached) = crate::storage::rate_limit_cache::read_cache() {
                if let Ok(mut guard) = self.cache.lock() {
                    *guard = Some(CacheEntry {
                        stats: cached.stats.clone(),
                        timestamp: Instant::now(),
                    });
                }
                return Some(cached.stats);
            }

            // Return stale memory cache
            self.cache
                .lock()
                .ok()
                .and_then(|guard| guard.as_ref().map(|e| e.stats.clone()))
        }
    }

    /// Format the second-line display
    fn format_display(&self, stats: &UsageStats, ctx: &RenderContext) -> String {
        let mut parts = Vec::new();

        let rate_icon = select_icon(
            ctx,
            &self.config.display.emoji_icon,
            &self.config.display.nerd_icon,
            &self.config.display.text_icon,
        );
        let timer_icon = select_icon(
            ctx,
            &self.config.display.timer_emoji_icon,
            &self.config.display.timer_nerd_icon,
            &self.config.display.timer_text_icon,
        );

        // 5h token usage section
        if let Some(token) = &stats.token_usage {
            let color = self.status_color(token.percentage);
            let segment =
                self.format_token_segment(token, "5h", &rate_icon, &timer_icon, ctx, color);
            parts.push(segment);
        }

        // 7d token usage section
        if let Some(weekly) = &stats.weekly_token_usage {
            let color = self.status_color(weekly.percentage);
            let segment =
                self.format_token_segment(weekly, "7d", &rate_icon, &timer_icon, ctx, color);
            parts.push(segment);
        }

        // MCP usage section
        if let Some(mcp) = &stats.mcp_usage {
            let color = self.status_color(mcp.percentage);
            parts.push(format!(
                "\x1b[38;5;{color}m\u{1f310} MCP: {}/{} ({})\x1b[0m",
                mcp.used, mcp.limit, mcp.time_window
            ));
        }

        if parts.is_empty() {
            String::new()
        } else {
            parts.join(" \u{2502} ")
        }
    }

    /// 渲染单个 token 用量段（5h 或 7d）
    fn format_token_segment(
        &self,
        usage: &crate::api::QuotaUsage,
        label: &str,
        rate_icon: &str,
        timer_icon: &str,
        ctx: &RenderContext,
        color: u8,
    ) -> String {
        let mut token_parts = Vec::new();

        // Percentage
        token_parts.push(format!("{}%", usage.percentage));

        // Progress bar (5h 和 7d 独立控制)
        let show_bar = match label {
            "5h" => self.config.display.show_5h_progress_bar,
            "7d" => self.config.display.show_7d_progress_bar,
            _ => true,
        };
        if show_bar {
            let width = self.config.display.progress_width.max(1) as usize;
            let params = ProgressBarParams {
                percentage: f64::from(usage.percentage),
                width,
                filled_char: '\u{2588}', // █
                empty_char: '\u{2591}',  // ░
                backup_char: '\u{2593}', // ▓
                backup_threshold: f64::from(self.config.glm_usage.thresholds.danger),
                gradient_enabled: true,
                supports_colors: ctx.terminal.supports_colors(),
                baseline_percentage: None,
            };

            if let Some(bar) = progress_bar::build_progress_bar(&params) {
                token_parts.push(format!("[{bar}]"));
            }
        }

        // Countdown
        if self.config.display.show_countdown {
            if let Some(countdown) = format_countdown(usage.reset_at) {
                token_parts.push(format!("{timer_icon} {countdown}"));
            }
        }

        let token_text = token_parts.join(" ");
        format!("\x1b[38;5;{color}m{rate_icon} {label}: {token_text}\x1b[0m")
    }

    /// Get ANSI 256 color code based on usage percentage
    const fn status_color(&self, percentage: u8) -> u8 {
        let thresholds = &self.config.glm_usage.thresholds;
        if percentage >= thresholds.danger {
            196 // Red
        } else if percentage >= thresholds.warning {
            226 // Yellow
        } else {
            109 // Green
        }
    }
}

/// Calculate countdown to reset time, format as Xd Xh or Xh Xm
fn format_countdown(reset_at: Option<i64>) -> Option<String> {
    let reset_secs = reset_at?;
    #[allow(clippy::cast_possible_wrap)] // u64 -> i64 安全，当前 epoch 远小于 i64::MAX
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;

    let remaining = reset_secs.saturating_sub(now);

    if remaining <= 0 {
        return Some("0m".to_string());
    }

    let total_hours = remaining / 3600;
    let minutes = (remaining % 3600) / 60;

    if total_hours >= 24 {
        let days = total_hours / 24;
        let hours = total_hours % 24;
        Some(format!("{days}d{hours}h"))
    } else {
        Some(format!("{total_hours}h{minutes}m"))
    }
}

/// 根据终端能力选择合适的图标（复用与 `Component::select_icon` 相同的优先级）
fn select_icon(ctx: &RenderContext, emoji: &str, nerd: &str, text: &str) -> String {
    // 强制模式优先
    if ctx.config.terminal.force_text {
        return text.to_string();
    }
    if ctx.config.terminal.force_nerd_font {
        return nerd.to_string();
    }
    if ctx.config.terminal.force_emoji {
        return emoji.to_string();
    }

    // 自动检测
    if ctx.terminal.supports_nerd_font && ctx.config.style.enable_nerd_font.is_enabled(true) {
        nerd.to_string()
    } else if ctx.terminal.supports_emoji && ctx.config.style.enable_emoji.is_enabled(true) {
        emoji.to_string()
    } else {
        text.to_string()
    }
}

#[async_trait]
impl Component for RateLimitComponent {
    fn name(&self) -> &'static str {
        "rate_limit"
    }

    fn is_enabled(&self, _ctx: &RenderContext) -> bool {
        self.config.enabled
    }

    async fn render(&self, ctx: &RenderContext) -> ComponentOutput {
        if !self.is_enabled(ctx) {
            return ComponentOutput::hidden();
        }

        let Some(stats) = self.get_usage_stats() else {
            return ComponentOutput::hidden();
        };

        let text = self.format_display(&stats, ctx);

        if text.is_empty() {
            return ComponentOutput::hidden();
        }

        ComponentOutput::new(text).with_component_name("rate_limit")
    }

    fn base_config(&self, _ctx: &RenderContext) -> Option<&crate::config::BaseComponentConfig> {
        None // rate_limit doesn't use the standard base config pattern
    }
}

/// Factory for creating `RateLimit` components
pub struct RateLimitComponentFactory;

impl ComponentFactory for RateLimitComponentFactory {
    fn create(&self, config: &Config) -> Box<dyn Component> {
        Box::new(RateLimitComponent::new(
            config.components.rate_limit.clone(),
        ))
    }

    fn name(&self) -> &'static str {
        "rate_limit"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::QuotaUsage;
    use crate::components::TerminalCapabilities;
    use std::sync::Arc;

    fn test_config() -> RateLimitComponentConfig {
        RateLimitComponentConfig::default()
    }

    fn test_context() -> RenderContext {
        RenderContext {
            input: Arc::new(crate::core::InputData::default()),
            config: Arc::new(crate::config::Config::default()),
            terminal: TerminalCapabilities::default(),
        }
    }

    fn make_stats(token_pct: u8, mcp_used: i64, mcp_limit: i64) -> UsageStats {
        UsageStats {
            token_usage: Some(QuotaUsage {
                used: i64::from(token_pct) * 10,
                limit: 1000,
                percentage: token_pct,
                time_window: "5h".to_string(),
                reset_at: Some(1_700_000_000),
            }),
            mcp_usage: Some(QuotaUsage {
                used: mcp_used,
                limit: mcp_limit,
                percentage: if mcp_limit > 0 {
                    #[allow(
                        clippy::cast_possible_truncation,
                        clippy::cast_sign_loss,
                        clippy::cast_precision_loss
                    )]
                    {
                        ((mcp_used as f64 / mcp_limit as f64) * 100.0).clamp(0.0, 100.0) as u8
                    }
                } else {
                    0
                },
                time_window: "30d".to_string(),
                reset_at: None,
            }),
            weekly_token_usage: None,
        }
    }

    #[test]
    fn test_status_color_green() {
        let component = RateLimitComponent::new(test_config());
        assert_eq!(component.status_color(50), 109); // Green
    }

    #[test]
    fn test_status_color_yellow() {
        let component = RateLimitComponent::new(test_config());
        assert_eq!(component.status_color(85), 226); // Yellow (>= 80)
    }

    #[test]
    fn test_status_color_red() {
        let component = RateLimitComponent::new(test_config());
        assert_eq!(component.status_color(96), 196); // Red (>= 95)
    }

    #[test]
    fn test_format_display_with_data() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        let stats = make_stats(32, 15, 100);

        let result = component.format_display(&stats, &ctx);
        assert!(result.contains("5h"), "Should contain 5h token section");
        assert!(result.contains("32%"), "Should show percentage");
        assert!(result.contains("MCP"), "Should contain MCP section");
        assert!(result.contains("15/100"), "Should show MCP usage");
    }

    #[test]
    fn test_format_display_token_only() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        let stats = UsageStats {
            token_usage: Some(QuotaUsage {
                used: 320,
                limit: 1000,
                percentage: 32,
                time_window: "5h".to_string(),
                reset_at: None,
            }),
            mcp_usage: None,
            weekly_token_usage: None,
        };

        let result = component.format_display(&stats, &ctx);
        assert!(result.contains("5h"), "Should contain 5h section");
        assert!(!result.contains("MCP"), "Should not contain MCP section");
    }

    #[test]
    fn test_format_display_empty_stats() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        let stats = UsageStats {
            token_usage: None,
            mcp_usage: None,
            weekly_token_usage: None,
        };

        let result = component.format_display(&stats, &ctx);
        assert!(result.is_empty(), "Should be empty with no data");
    }

    #[test]
    fn test_component_enabled_by_default() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        assert!(component.is_enabled(&ctx));
    }

    #[test]
    fn test_component_disabled() {
        let mut config = test_config();
        config.enabled = false;
        let component = RateLimitComponent::new(config);
        let ctx = test_context();
        assert!(!component.is_enabled(&ctx));
    }

    #[test]
    fn test_glm_usage_disabled() {
        let mut config = test_config();
        config.glm_usage.enabled = false;
        let component = RateLimitComponent::new(config);
        assert!(component.get_usage_stats().is_none());
    }

    #[test]
    fn test_format_countdown_expired() {
        let result = format_countdown(Some(0));
        assert_eq!(result, Some("0m".to_string()));
    }

    #[test]
    fn test_format_countdown_none() {
        let result = format_countdown(None);
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_render_hidden_when_disabled() {
        let mut config = test_config();
        config.enabled = false;
        let component = RateLimitComponent::new(config);
        let ctx = test_context();
        let output = component.render(&ctx).await;
        assert!(!output.visible);
    }

    #[tokio::test]
    async fn test_render_hidden_when_no_env() {
        // With glm_usage disabled, no API call is attempted
        let mut config = test_config();
        config.glm_usage.enabled = false;
        let component = RateLimitComponent::new(config);
        let ctx = test_context();
        let output = component.render(&ctx).await;
        assert!(!output.visible, "Should be hidden when glm_usage disabled");
    }

    #[test]
    fn test_factory_name() {
        let factory = RateLimitComponentFactory;
        assert_eq!(factory.name(), "rate_limit");
    }

    #[test]
    fn test_factory_creates_component() {
        let factory = RateLimitComponentFactory;
        let config = crate::config::Config::default();
        let component = factory.create(&config);
        assert_eq!(component.name(), "rate_limit");
    }

    // ===== 新增测试 =====

    #[test]
    fn test_status_color_custom_thresholds() {
        let mut config = test_config();
        config.glm_usage.thresholds.warning = 70;
        config.glm_usage.thresholds.danger = 90;
        let component = RateLimitComponent::new(config);

        assert_eq!(component.status_color(60), 109); // <70: green
        assert_eq!(component.status_color(75), 226); // 70-89: yellow
        assert_eq!(component.status_color(92), 196); // >=90: red
    }

    #[test]
    fn test_status_color_boundary_warning() {
        let component = RateLimitComponent::new(test_config());
        // Exactly at warning threshold (80)
        assert_eq!(component.status_color(80), 226); // yellow
                                                     // Just below warning
        assert_eq!(component.status_color(79), 109); // green
    }

    #[test]
    fn test_status_color_boundary_danger() {
        let component = RateLimitComponent::new(test_config());
        // Exactly at danger threshold (95)
        assert_eq!(component.status_color(95), 196); // red
                                                     // Just below danger
        assert_eq!(component.status_color(94), 226); // yellow
    }

    #[test]
    fn test_format_display_with_progress_bar_disabled() {
        let mut config = test_config();
        config.display.show_5h_progress_bar = false;
        let component = RateLimitComponent::new(config);
        let ctx = test_context();
        let stats = make_stats(50, 10, 100);

        let result = component.format_display(&stats, &ctx);
        assert!(result.contains("50%"));
        // No progress bar → no [█...] or [░...] pattern
        let stripped = strip_ansi(&result);
        assert!(
            !stripped.contains("[█") && !stripped.contains("[░") && !stripped.contains("[▓"),
            "Should not contain progress bar when disabled"
        );
    }

    /// 去除 ANSI escape 序列的辅助函数
    fn strip_ansi(s: &str) -> String {
        let mut result = String::with_capacity(s.len());
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                // 跳过 escape 序列
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&next) = chars.peek() {
                        chars.next();
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            } else {
                result.push(c);
            }
        }
        result
    }

    #[test]
    fn test_format_display_with_countdown_disabled() {
        let mut config = test_config();
        config.display.show_countdown = false;
        let component = RateLimitComponent::new(config);
        let ctx = test_context();
        let stats = make_stats(50, 10, 100);

        let result = component.format_display(&stats, &ctx);
        assert!(result.contains("50%"));
        // 倒计时禁用时不显示倒计时图标
        let stripped = strip_ansi(&result);
        assert!(
            !stripped.contains('\u{23f0}'),
            "Should not contain timer icon when countdown disabled"
        );
    }

    #[test]
    fn test_format_display_mcp_only() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        let stats = UsageStats {
            token_usage: None,
            mcp_usage: Some(QuotaUsage {
                used: 30,
                limit: 50,
                percentage: 60,
                time_window: "30d".to_string(),
                reset_at: None,
            }),
            weekly_token_usage: None,
        };

        let result = component.format_display(&stats, &ctx);
        assert!(!result.contains("5h"), "Should not contain 5h section");
        assert!(result.contains("MCP"), "Should contain MCP section");
        assert!(result.contains("30/50"), "Should show MCP usage");
    }

    #[test]
    fn test_format_display_ansi_colors() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        let stats = make_stats(90, 80, 100); // both yellow/danger range

        let result = component.format_display(&stats, &ctx);
        // Should contain ANSI 256 color codes
        assert!(result.contains("\x1b[38;5;"), "Should contain ANSI colors");
        assert!(result.contains("\x1b[0m"), "Should contain reset");
    }

    #[test]
    fn test_format_display_separator() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        let stats = make_stats(50, 10, 100);

        let result = component.format_display(&stats, &ctx);
        // Both token and MCP present → should have │ separator
        assert!(result.contains('\u{2502}'), "Should contain │ separator");
    }

    #[test]
    fn test_format_countdown_normal() {
        // Create a reset_at that is 2h30m in the future
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map_or(0, |d| d.as_secs());
        #[allow(clippy::cast_possible_wrap)]
        let now = now as i64;
        let reset_at = now + 2 * 3600 + 30 * 60;

        let result = format_countdown(Some(reset_at));
        assert!(result.is_some());
        let countdown = result.as_deref().unwrap_or("");
        assert!(countdown.contains('2'), "Should show ~2 hours");
        assert!(countdown.contains('h'), "Should be Xh Xm format");
    }

    #[test]
    fn test_get_usage_stats_returns_none_when_no_env() {
        // Without ANTHROPIC_AUTH_TOKEN set, should return None (no file cache either)
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        let component = RateLimitComponent::new(test_config());
        // This will try API → fail → file cache → fail → stale memory → None
        let result = component.get_usage_stats();
        // Result depends on whether file cache exists, but most likely None
        // Just ensure it doesn't panic
        drop(result);
    }

    #[test]
    fn test_memory_cache_hit() {
        let component = RateLimitComponent::new(test_config());
        let stats = make_stats(50, 10, 100);

        // Manually populate memory cache
        {
            if let Ok(mut guard) = component.cache.lock() {
                *guard = Some(CacheEntry {
                    stats,
                    timestamp: std::time::Instant::now(),
                });
            }
        }

        let result = component.get_usage_stats();
        assert!(result.is_some());
        assert_eq!(
            result
                .as_ref()
                .and_then(|s| s.token_usage.as_ref())
                .map_or(0, |u| u.percentage),
            50
        );
    }

    #[test]
    fn test_memory_cache_expired_returns_data() {
        // 测试缓存过期时的行为：API 不可用时仍然能返回数据（来自文件缓存或陈旧内存）
        let mut config = test_config();
        config.glm_usage.cache_ttl = 0; // TTL = 0 → always expired
        let component = RateLimitComponent::new(config);
        let stats = make_stats(50, 10, 100);

        // Populate with old timestamp (cache should be expired)
        {
            if let Ok(mut guard) = component.cache.lock() {
                *guard = Some(CacheEntry {
                    stats,
                    timestamp: std::time::Instant::now()
                        .checked_sub(std::time::Duration::from_secs(100))
                        .unwrap_or_else(std::time::Instant::now),
                });
            }
        }

        // Will try API (fails without env) → file cache → stale memory
        std::env::remove_var("ANTHROPIC_AUTH_TOKEN");
        let result = component.get_usage_stats();
        // 关键断言：不应该 panic，且应该返回 Some（来自文件缓存或陈旧内存）
        assert!(
            result.is_some(),
            "Should return data from fallback (file or stale memory cache)"
        );
    }

    #[tokio::test]
    async fn test_render_visible_with_cached_data() {
        let component = RateLimitComponent::new(test_config());
        let stats = make_stats(32, 15, 100);

        // Pre-populate cache
        {
            if let Ok(mut guard) = component.cache.lock() {
                *guard = Some(CacheEntry {
                    stats,
                    timestamp: std::time::Instant::now(),
                });
            }
        }

        let ctx = test_context();
        let output = component.render(&ctx).await;
        assert!(output.visible, "Should be visible with cached data");
        assert!(output.text.contains("32%"), "Should show percentage");
        assert_eq!(output.component_name.as_deref(), Some("rate_limit"));
    }

    // ===== icon 选择测试 =====

    #[test]
    fn test_select_icon_force_text() {
        let mut config = crate::config::Config::default();
        config.terminal.force_text = true;
        let ctx = RenderContext {
            input: Arc::new(crate::core::InputData::default()),
            config: Arc::new(config),
            terminal: TerminalCapabilities::default(),
        };
        let result = select_icon(&ctx, "emoji", "nerd", "text");
        assert_eq!(result, "text");
    }

    #[test]
    fn test_select_icon_force_nerd() {
        let mut config = crate::config::Config::default();
        config.terminal.force_nerd_font = true;
        let ctx = RenderContext {
            input: Arc::new(crate::core::InputData::default()),
            config: Arc::new(config),
            terminal: TerminalCapabilities::default(),
        };
        let result = select_icon(&ctx, "emoji", "nerd", "text");
        assert_eq!(result, "nerd");
    }

    #[test]
    fn test_select_icon_force_emoji() {
        let mut config = crate::config::Config::default();
        config.terminal.force_emoji = true;
        let ctx = RenderContext {
            input: Arc::new(crate::core::InputData::default()),
            config: Arc::new(config),
            terminal: TerminalCapabilities {
                supports_nerd_font: true,
                ..TerminalCapabilities::default()
            },
        };
        let result = select_icon(&ctx, "emoji", "nerd", "text");
        assert_eq!(result, "emoji");
    }

    #[test]
    fn test_select_icon_auto_nerd() {
        let ctx = RenderContext {
            input: Arc::new(crate::core::InputData::default()),
            config: Arc::new(crate::config::Config::default()),
            terminal: TerminalCapabilities {
                supports_nerd_font: true,
                supports_emoji: true,
                ..TerminalCapabilities::default()
            },
        };
        let result = select_icon(&ctx, "emoji", "nerd", "text");
        assert_eq!(result, "nerd");
    }

    #[test]
    fn test_select_icon_auto_emoji() {
        let mut config = crate::config::Config::default();
        config.style.enable_nerd_font = crate::config::AutoDetect::Bool(false);
        let ctx = RenderContext {
            input: Arc::new(crate::core::InputData::default()),
            config: Arc::new(config),
            terminal: TerminalCapabilities {
                supports_nerd_font: false,
                supports_emoji: true,
                ..TerminalCapabilities::default()
            },
        };
        let result = select_icon(&ctx, "emoji", "nerd", "text");
        assert_eq!(result, "emoji");
    }

    #[test]
    fn test_select_icon_fallback_text() {
        let mut config = crate::config::Config::default();
        config.style.enable_emoji = crate::config::AutoDetect::Bool(false);
        config.style.enable_nerd_font = crate::config::AutoDetect::Bool(false);
        let ctx = RenderContext {
            input: Arc::new(crate::core::InputData::default()),
            config: Arc::new(config),
            terminal: TerminalCapabilities {
                supports_nerd_font: false,
                supports_emoji: false,
                ..TerminalCapabilities::default()
            },
        };
        let result = select_icon(&ctx, "emoji", "nerd", "text");
        assert_eq!(result, "text");
    }

    // ===== 7d 段显示/隐藏测试 =====

    #[test]
    fn test_format_display_with_weekly() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        let stats = UsageStats {
            token_usage: Some(QuotaUsage {
                used: 320,
                limit: 1000,
                percentage: 32,
                time_window: "5h".to_string(),
                reset_at: None,
            }),
            mcp_usage: None,
            weekly_token_usage: Some(QuotaUsage {
                used: 500,
                limit: 10000,
                percentage: 5,
                time_window: "7d".to_string(),
                reset_at: None,
            }),
        };

        let result = component.format_display(&stats, &ctx);
        assert!(result.contains("5h"), "Should contain 5h section");
        assert!(result.contains("7d"), "Should contain 7d section");
        assert!(result.contains("5%"), "Should show 7d percentage");
    }

    #[test]
    fn test_format_display_without_weekly() {
        let component = RateLimitComponent::new(test_config());
        let ctx = test_context();
        let stats = make_stats(50, 10, 100);

        let result = component.format_display(&stats, &ctx);
        assert!(!result.contains("7d"), "Should not contain 7d section");
    }

    // ===== countdown 天格式测试 =====

    #[test]
    fn test_format_countdown_days() {
        // 53 小时 = 2d 5h
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map_or(0, |d| d.as_secs());
        #[allow(clippy::cast_possible_wrap)]
        let now = now as i64;
        let reset_at = now + 53 * 3600;

        let result = format_countdown(Some(reset_at));
        assert_eq!(result, Some("2d5h".to_string()));
    }

    #[test]
    fn test_format_countdown_under_one_hour() {
        // 42 分钟
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map_or(0, |d| d.as_secs());
        #[allow(clippy::cast_possible_wrap)]
        let now = now as i64;
        let reset_at = now + 42 * 60;

        let result = format_countdown(Some(reset_at));
        assert!(result.is_some());
        let countdown = result.as_deref().unwrap_or("");
        assert!(countdown.contains("0h"), "Should show 0h");
        assert!(countdown.contains("42m"), "Should show 42m");
    }
}
