//! Core statusline generator
//!
//! The main orchestrator that coordinates components, themes, and terminal rendering.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::components::{ComponentFactory, ComponentOutput, RenderContext, TerminalCapabilities};
use crate::config::Config;
use crate::core::{InputData, MultiLineRenderer};
use crate::storage::{self, ProjectResolver};
use crate::terminal::detector::TerminalDetector;
use crate::themes::{create_theme_renderer, ThemeRenderer};

const POWERLINE_PALETTE: &[(&str, &str)] = &[
    ("project", "blue"),
    ("model", "cyan"),
    ("branch", "green"),
    ("tokens", "yellow"),
    ("usage", "orange"),
    ("status", "magenta"),
    ("rate_limit", "cyan"),
];

const CAPSULE_PALETTE: &[(&str, &str)] = &[
    ("project", "bright_blue"),
    ("model", "cyan"),
    ("branch", "bright_green"),
    ("tokens", "yellow"),
    ("usage", "bright_orange"),
    ("status", "bright_magenta"),
    ("rate_limit", "cyan"),
];

/// 去除文本中的 ANSI 256 色前景码（`\x1b[38;5;NNm`），
/// 保留真彩色码（`\x1b[38;2;R;G;Bm`，用于进度条渐变）和 reset 码。
fn strip_ansi_256_fg(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remaining = text;

    while let Some(pos) = remaining.find("\x1b[38;5;") {
        // 保留 escape 序列之前的内容
        result.push_str(&remaining[..pos]);
        // 找到 'm' 结束符
        let after_prefix = &remaining[pos + 7..]; // 跳过 "\x1b[38;5;"
        if let Some(end) = after_prefix.find('m') {
            let digits = &after_prefix[..end];
            if digits.chars().all(|c| c.is_ascii_digit()) {
                // 有效的 256 色码，跳过
                remaining = &after_prefix[end + 1..];
            } else {
                result.push_str(&remaining[..pos + 7]);
                remaining = &remaining[pos + 7..];
            }
        } else {
            result.push_str(remaining);
            remaining = "";
        }
    }

    result.push_str(remaining);
    result
}

/// Generator options
#[derive(Debug, Clone)]
pub struct GeneratorOptions {
    /// Override preset configuration
    pub preset: Option<String>,
    /// Enable update throttling (default: true)
    pub update_throttling: bool,
    /// Disable caching
    pub disable_cache: bool,
    /// Base directory for configuration
    pub config_base_dir: Option<String>,
}

impl Default for GeneratorOptions {
    fn default() -> Self {
        Self {
            preset: None,
            update_throttling: true,
            disable_cache: false,
            config_base_dir: None,
        }
    }
}

impl GeneratorOptions {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_preset(mut self, preset: String) -> Self {
        self.preset = Some(preset);
        self
    }
}

/// Core statusline generator
///
/// Integrates all components to generate the final statusline
pub struct StatuslineGenerator {
    config: Arc<Config>,
    component_registry: HashMap<String, Box<dyn ComponentFactory>>,
    terminal_detector: TerminalDetector,
    theme_renderer: Box<dyn ThemeRenderer>,
    multi_line_renderer: MultiLineRenderer,
    last_update: Option<Instant>,
    last_result: Option<String>,
    update_interval: Duration,
    disable_cache: bool,
    storage_initialized: bool,
    active_project_id: Option<String>,
    config_base_dir: Option<PathBuf>,
}

impl StatuslineGenerator {
    /// Create a new generator with the given configuration and options
    pub fn new(config: Config, options: GeneratorOptions) -> Self {
        let config_arc = Arc::new(config);
        let terminal_detector = TerminalDetector::new();

        // Create theme renderer based on configuration
        let theme_renderer = create_theme_renderer(&config_arc.theme);

        let config_base_dir = options.config_base_dir.clone().map(PathBuf::from);
        let multi_line_renderer =
            MultiLineRenderer::new((*config_arc).clone(), config_base_dir.clone());

        // Set update interval based on options
        let update_interval = if options.update_throttling {
            Duration::from_millis(300) // Official 300ms update interval
        } else {
            Duration::from_millis(0)
        };

        let mut generator = Self {
            config: Arc::clone(&config_arc),
            component_registry: HashMap::new(),
            terminal_detector,
            theme_renderer,
            multi_line_renderer,
            last_update: None,
            last_result: None,
            update_interval,
            disable_cache: options.disable_cache,
            storage_initialized: false,
            active_project_id: None,
            config_base_dir,
        };
        drop(config_arc);

        // Apply preset if specified
        generator.apply_config_preset();
        if let Some(preset) = options.preset {
            generator.apply_preset(&preset);
        }

        // Initialize components
        generator.initialize_components();

        // Ensure multiline renderer holds latest config state
        generator.refresh_multiline_renderer();

        generator
    }

    /// Initialize component registry
    fn initialize_components(&mut self) {
        use crate::components::{
            BranchComponentFactory, ModelComponentFactory, ProjectComponentFactory,
            RateLimitComponentFactory, StatusComponentFactory, TokensComponentFactory,
            UsageComponentFactory,
        };

        // Register all component factories
        self.component_registry
            .insert("project".to_string(), Box::new(ProjectComponentFactory));
        self.component_registry
            .insert("model".to_string(), Box::new(ModelComponentFactory));
        self.component_registry
            .insert("branch".to_string(), Box::new(BranchComponentFactory));
        self.component_registry
            .insert("tokens".to_string(), Box::new(TokensComponentFactory));
        self.component_registry
            .insert("status".to_string(), Box::new(StatusComponentFactory));
        self.component_registry
            .insert("usage".to_string(), Box::new(UsageComponentFactory));
        self.component_registry.insert(
            "rate_limit".to_string(),
            Box::new(RateLimitComponentFactory),
        );
    }

    fn refresh_multiline_renderer(&mut self) {
        let base_dir = self.config_base_dir.clone();
        self.multi_line_renderer
            .update_config((*self.config).clone(), base_dir);
    }

    /// Apply a preset configuration
    fn apply_preset(&mut self, preset: &str) {
        // Parse preset string (e.g., "PMBTUS" -> ["P", "M", "B", "T", "U", "S"])
        let component_map = Self::parse_preset(preset);

        // Update config.components.order based on preset
        if let Some(ref mut config) = Arc::get_mut(&mut self.config) {
            config.components.order = component_map;
        }

        self.refresh_multiline_renderer();
    }

    /// Apply preset defined in configuration if present
    fn apply_config_preset(&mut self) {
        if self.config.components.order.is_empty() {
            if let Some(preset) = self.config.preset.clone() {
                self.apply_preset(&preset);
            }
        }

        self.refresh_multiline_renderer();
    }

    /// Parse preset string into component order
    fn parse_preset(preset: &str) -> Vec<String> {
        let mut seen = HashSet::new();

        preset
            .chars()
            .filter_map(|c| match c.to_ascii_uppercase() {
                'P' => Some("project"),
                'M' => Some("model"),
                'B' => Some("branch"),
                'T' => Some("tokens"),
                'U' => Some("usage"),
                'S' => Some("status"),
                'R' => Some("rate_limit"),
                _ => None,
            })
            .filter(|name| seen.insert(*name))
            .map(std::string::ToString::to_string)
            .collect()
    }

    /// Check if update should be performed based on throttling
    fn should_update(&mut self) -> bool {
        if self.disable_cache || self.update_interval.as_millis() == 0 {
            return true;
        }

        match self.last_update {
            None => {
                self.last_update = Some(Instant::now());
                true
            }
            Some(last) => {
                let now = Instant::now();
                if now.duration_since(last) >= self.update_interval {
                    self.last_update = Some(now);
                    true
                } else {
                    false
                }
            }
        }
    }

    /// Generate the statusline
    /// # Errors
    ///
    /// Returns an error if component rendering fails or if required
    /// configuration initialization steps cannot complete successfully.
    pub async fn generate(&mut self, input_data: InputData) -> Result<String> {
        // Check update rate limit
        self.ensure_storage_ready(&input_data).await?;

        if let Ok(snapshot_value) = serde_json::to_value(&input_data) {
            if let Err(err) = storage::update_session_snapshot(&snapshot_value).await {
                // Only log unexpected errors; missing session ID is expected in some scenarios
                if !err.to_string().contains("No session ID found") {
                    eprintln!("[statusline] failed to update session snapshot: {err}");
                }
            }
        }

        if !self.should_update() {
            if let Some(ref last_result) = self.last_result {
                return Ok(last_result.clone());
            }
        }

        // Detect terminal capabilities
        let capabilities = self.detect_terminal_capabilities();

        // Create render context
        let context = RenderContext {
            input: Arc::new(input_data),
            config: self.config.clone(),
            terminal: capabilities,
        };

        // Render components
        let component_results = self.render_components(&context).await?;

        // Separate rate_limit output for independent line rendering
        let (main_components, rate_limit_output): (Vec<_>, Vec<_>) = component_results
            .into_iter()
            .partition(|c| c.component_name.as_deref() != Some("rate_limit"));

        // Apply theme rendering to main line components
        let colors = self.extract_component_colors(&main_components);
        let main_line = self
            .theme_renderer
            .render(&main_components, &colors, &context)?;

        // Render multiline extensions
        let extension_result = self
            .multi_line_renderer
            .render_extension_lines(&context)
            .await;

        let mut lines = Vec::new();
        if !main_line.is_empty() {
            lines.push(main_line);
        }

        if extension_result.success {
            lines.extend(extension_result.lines);
        } else if let Some(err) = extension_result.error {
            eprintln!("[statusline] multiline render failed: {err}");
        }

        // 将 rate_limit 通过主题渲染器包装后作为独立行
        if !rate_limit_output.is_empty() {
            let rl_colors = self.extract_component_colors(&rate_limit_output);
            // 去除 rate_limit 自带的 256 色前景码，让主题的前景色（白色）生效
            let stripped_rl: Vec<ComponentOutput> = rate_limit_output
                .into_iter()
                .map(|mut rl| {
                    rl.text = strip_ansi_256_fg(&rl.text);
                    rl
                })
                .collect();
            let rl_line = self
                .theme_renderer
                .render(&stripped_rl, &rl_colors, &context)?;
            if !rl_line.is_empty() {
                lines.push(rl_line);
            }
        }

        let result = lines.join("\n");

        // Cache result
        if !self.disable_cache {
            self.last_result = Some(result.clone());
        }

        Ok(result)
    }

    fn extract_component_colors(&self, components: &[ComponentOutput]) -> Vec<String> {
        let mut colors = Vec::with_capacity(components.len());
        let theme_palette = match self.config.theme.as_str() {
            "powerline" => Some(POWERLINE_PALETTE),
            "capsule" => Some(CAPSULE_PALETTE),
            _ => None,
        };

        for component in components {
            let Some(name) = component.component_name.as_deref() else {
                continue;
            };

            let color = theme_palette
                .and_then(|palette| {
                    palette
                        .iter()
                        .find(|(component_name, _)| *component_name == name)
                        .map(|(_, color)| (*color).to_string())
                })
                .unwrap_or_else(|| self.component_config_color(name));

            colors.push(color);
        }

        colors
    }

    fn component_config_color(&self, name: &str) -> String {
        match name {
            "project" => self.config.components.project.base.icon_color.clone(),
            "model" => self.config.components.model.base.icon_color.clone(),
            "branch" => self.config.components.branch.base.icon_color.clone(),
            "tokens" => self.config.components.tokens.base.icon_color.clone(),
            "usage" => self.config.components.usage.base.icon_color.clone(),
            "status" => self.config.components.status.base.icon_color.clone(),
            "rate_limit" => "cyan".to_string(),
            other => {
                eprintln!(
                    "[statusline] unknown component '{other}' when resolving theme colors, fallback to blue"
                );
                "blue".to_string()
            }
        }
    }

    /// Detect terminal capabilities
    fn detect_terminal_capabilities(&self) -> TerminalCapabilities {
        let caps = self.terminal_detector.detect(
            &self.config.style.enable_colors,
            &self.config.style.enable_emoji,
            &self.config.style.enable_nerd_font,
            self.config.terminal.force_nerd_font,
            self.config.terminal.force_emoji,
            self.config.terminal.force_text,
        );

        if self.config.debug {
            eprintln!("[调试] 终端能力检测结果:");
            eprintln!("  - color_support: {:?}", caps.color_support);
            eprintln!("  - supports_emoji: {}", caps.supports_emoji);
            eprintln!("  - supports_nerd_font: {}", caps.supports_nerd_font);
            eprintln!("  - TERM_PROGRAM: {:?}", std::env::var("TERM_PROGRAM"));
        }

        caps
    }

    /// Render all enabled components
    async fn render_components(&self, context: &RenderContext) -> Result<Vec<ComponentOutput>> {
        let mut results = Vec::new();

        // Get component order from configuration or use default
        let default_order = vec![
            "project".to_string(),
            "model".to_string(),
            "branch".to_string(),
            "tokens".to_string(),
            "usage".to_string(),
            "status".to_string(),
            "rate_limit".to_string(),
        ];

        let component_order = if self.config.components.order.is_empty() {
            default_order
        } else {
            self.config.components.order.clone()
        };

        // Render each component in order
        let mut seen = HashSet::new();
        for component_name in &component_order {
            if !seen.insert(component_name.clone()) {
                continue;
            }

            let Some(factory) = self.component_registry.get(component_name.as_str()) else {
                continue;
            };

            let component = factory.create(&self.config);
            if !component.is_enabled(context) {
                continue;
            }

            let mut output = component.render(context).await;
            if !output.visible {
                continue;
            }

            output.set_component_name(component_name.clone());
            results.push(output);
        }

        Ok(results)
    }

    async fn ensure_storage_ready(&mut self, input_data: &InputData) -> Result<()> {
        if let Some(transcript) = input_data.transcript_path.as_deref() {
            ProjectResolver::set_global_project_id_from_transcript(Some(transcript));
        }

        let fallback_path = input_data.project_dir().or(input_data.cwd.as_deref());

        let project_id = ProjectResolver::get_global_project_id(fallback_path);
        ProjectResolver::set_global_project_id(Some(&project_id));

        if !self.storage_initialized
            || self.active_project_id.as_deref() != Some(project_id.as_str())
        {
            storage::initialize_storage_with_settings(
                Some(project_id.clone()),
                &self.config.storage,
            )
            .await?;
            self.storage_initialized = true;
            self.active_project_id = Some(project_id);
        }

        Ok(())
    }

    /// Get the current configuration
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Update configuration
    pub fn update_config(&mut self, config: Config) {
        self.config = Arc::new(config);
        self.apply_config_preset();
        self.theme_renderer = create_theme_renderer(&self.config.theme);
        self.refresh_multiline_renderer();
        // Clear cache to force re-render
        self.last_result = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::{ColorSupport, TerminalCapabilities};
    use std::sync::Arc;

    #[test]
    fn test_parse_preset() {
        let order = StatuslineGenerator::parse_preset("PMBT");
        assert_eq!(order, vec!["project", "model", "branch", "tokens"]);

        let order = StatuslineGenerator::parse_preset("TBMP");
        assert_eq!(order, vec!["tokens", "branch", "model", "project"]);

        // Test with lowercase and mixed case
        let order = StatuslineGenerator::parse_preset("pmBT");
        assert_eq!(order, vec!["project", "model", "branch", "tokens"]);

        // Test with invalid characters
        let order = StatuslineGenerator::parse_preset("PM-BT");
        assert_eq!(order, vec!["project", "model", "branch", "tokens"]);
    }

    #[test]
    fn test_generator_options() {
        let options = GeneratorOptions::new().with_preset("PMBT".to_string());

        assert_eq!(options.preset, Some("PMBT".to_string()));
        assert!(options.update_throttling);
        assert!(!options.disable_cache);
    }

    #[tokio::test]
    async fn test_generator_creation() {
        let config = Config::default();
        let generator = StatuslineGenerator::new(config, GeneratorOptions::default());

        assert_eq!(generator.update_interval, Duration::from_millis(300));
        assert!(!generator.disable_cache);
    }

    #[test]
    fn test_rate_limit_output_themed_via_render() {
        // 验证 rate_limit 的 ComponentOutput 能通过主题渲染器正确包装
        let rl_output = ComponentOutput::new("5h: ██████░░ 60%".to_string())
            .with_icon("R".to_string())
            .with_icon_color("cyan".to_string())
            .with_text_color("cyan".to_string());
        let components = vec![rl_output];
        let colors = vec!["cyan".to_string()];

        let config = Config::default();
        let context = RenderContext {
            input: Arc::new(InputData::default()),
            config: Arc::new(config),
            terminal: TerminalCapabilities {
                color_support: ColorSupport::TrueColor,
                supports_emoji: false,
                supports_nerd_font: true,
            },
        };

        // powerline 主题渲染
        let powerline = create_theme_renderer("powerline");
        let result = powerline
            .render(&components, &colors, &context)
            .unwrap_or_default();
        // powerline 应包含箭头分隔符
        assert!(result.contains('\u{e0b0}'), "powerline 应包含箭头分隔符");

        // capsule 主题渲染
        let capsule = create_theme_renderer("capsule");
        let result = capsule
            .render(&components, &colors, &context)
            .unwrap_or_default();
        // capsule 应包含左右帽符号
        assert!(result.contains('\u{e0b6}'), "capsule 应包含左帽符号");
        assert!(result.contains('\u{e0b4}'), "capsule 应包含右帽符号");

        // classic 主题不应包含特殊符号
        let classic = create_theme_renderer("classic");
        let result = classic
            .render(&components, &colors, &context)
            .unwrap_or_default();
        assert!(
            !result.contains('\u{e0b0}'),
            "classic 不应包含 powerline 符号"
        );
    }
}
