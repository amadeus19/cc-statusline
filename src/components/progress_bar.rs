//! Shared progress bar rendering module
//!
//! Provides reusable progress bar rendering functions for both
//! Tokens and `RateLimit` components.

use std::fmt::Write;

/// Rainbow gradient color from percentage
///
/// Returns an (R, G, B) tuple interpolated through soft color stops:
/// green → yellow-green → yellow → orange → red
#[must_use]
pub fn rainbow_gradient_color(percentage: f64) -> (u8, u8, u8) {
    let p = percentage.clamp(0.0, 100.0);

    let soft_green = (80.0, 200.0, 80.0);
    let soft_yellow_green = (150.0, 200.0, 60.0);
    let soft_yellow = (200.0, 200.0, 80.0);
    let soft_orange = (220.0, 160.0, 60.0);
    let soft_red = (200.0, 100.0, 80.0);

    let lerp = |start: (f64, f64, f64), end: (f64, f64, f64), t: f64| {
        let clamp_t = t.clamp(0.0, 1.0);
        (
            (end.0 - start.0).mul_add(clamp_t, start.0),
            (end.1 - start.1).mul_add(clamp_t, start.1),
            (end.2 - start.2).mul_add(clamp_t, start.2),
        )
    };

    let (r, g, b) = if p <= 25.0 {
        lerp(soft_green, soft_yellow_green, p / 25.0)
    } else if p <= 50.0 {
        lerp(soft_yellow_green, soft_yellow, (p - 25.0) / 25.0)
    } else if p <= 75.0 {
        lerp(soft_yellow, soft_orange, (p - 50.0) / 25.0)
    } else {
        lerp(soft_orange, soft_red, (p - 75.0) / 25.0)
    };

    let convert = |value: f64| -> u8 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            value.clamp(0.0, 255.0).round() as u8
        }
    };

    (convert(r), convert(g), convert(b))
}

/// Clamp a float value and round to usize
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use]
pub fn clamp_round_to_usize(value: f64, max: usize) -> usize {
    let max_f64 = to_f64(max);
    let clamped = value.clamp(0.0, max_f64);
    let rounded = clamped.round() as usize;
    rounded.min(max)
}

/// Convert a value implementing [`IntoF64`] to `f64`
#[must_use]
pub fn to_f64<T: IntoF64>(value: T) -> f64 {
    value.into_f64()
}

/// Trait for converting to f64 without precision loss warnings
pub trait IntoF64 {
    /// Convert to f64
    fn into_f64(self) -> f64;
}

impl IntoF64 for usize {
    #[allow(clippy::cast_precision_loss)]
    fn into_f64(self) -> f64 {
        self as f64
    }
}

impl IntoF64 for u64 {
    #[allow(clippy::cast_precision_loss)]
    fn into_f64(self) -> f64 {
        self as f64
    }
}

/// Parameters for building a progress bar
pub struct ProgressBarParams {
    /// The percentage value (0-100+)
    pub percentage: f64,
    /// Width of the progress bar in characters
    pub width: usize,
    /// Character for filled cells
    pub filled_char: char,
    /// Character for empty cells
    pub empty_char: char,
    /// Character for cells above backup threshold
    pub backup_char: char,
    /// Backup threshold percentage
    pub backup_threshold: f64,
    /// Whether to render gradient colors
    pub gradient_enabled: bool,
    /// Whether the terminal supports colors
    pub supports_colors: bool,
    /// 系统基线占用的百分比（启用时渲染三段式进度条）
    pub baseline_percentage: Option<f64>,
}

/// Build a progress bar string from parameters
///
/// Returns `None` if the width is 0.
#[must_use]
pub fn build_progress_bar(params: &ProgressBarParams) -> Option<String> {
    let width = params.width;
    if width == 0 {
        return None;
    }

    let width_f64 = to_f64(width);
    let filled_len = clamp_round_to_usize((params.percentage / 100.0) * width_f64, width);
    let capped_filled = filled_len.min(width);

    // 计算基线区域长度
    let baseline_len = params
        .baseline_percentage
        .filter(|&b| b > 0.0)
        .map_or(0, |b| clamp_round_to_usize((b / 100.0) * width_f64, width));

    let mut bar = String::with_capacity(width * 16);
    let mut color_active = false;

    for idx in 0..width {
        if idx < capped_filled {
            // 确定当前字符类型
            let (symbol, is_baseline_char) = if baseline_len > 0 && idx < baseline_len {
                // 基线区域：使用 backup_char（▓）
                (params.backup_char, true)
            } else {
                // 用户内容区域：根据阈值选择字符
                let gradient_percentage = if capped_filled == 0 {
                    0.0
                } else {
                    let idx_f64 = to_f64(idx);
                    let capped_filled_f64 = to_f64(capped_filled);
                    ((idx_f64 + 0.5) / capped_filled_f64) * params.percentage
                }
                .clamp(0.0, 100.0);
                let is_backup = gradient_percentage >= params.backup_threshold;
                (
                    if is_backup {
                        params.backup_char
                    } else {
                        params.filled_char
                    },
                    false,
                )
            };

            if params.gradient_enabled && params.supports_colors {
                // 基线区域使用灰色渐变
                let gradient_percentage = if is_baseline_char {
                    // 基线区域使用固定的暗色调
                    let base_p = (to_f64(idx) / width_f64) * 100.0;
                    (base_p * 0.3).clamp(0.0, 30.0) // 保持暗色
                } else if capped_filled == 0 {
                    0.0
                } else {
                    let idx_f64 = to_f64(idx);
                    let capped_filled_f64 = to_f64(capped_filled);
                    ((idx_f64 + 0.5) / capped_filled_f64) * params.percentage
                }
                .clamp(0.0, 100.0);
                let (r, g, b) = rainbow_gradient_color(gradient_percentage);
                let _ = write!(bar, "\x1b[38;2;{r};{g};{b}m{symbol}");
                color_active = true;
            } else {
                bar.push(symbol);
            }
        } else if params.gradient_enabled && params.supports_colors {
            bar.push_str("\x1b[38;2;120;120;120m");
            bar.push(params.empty_char);
            color_active = true;
        } else {
            bar.push(params.empty_char);
        }
    }

    if color_active {
        bar.push_str("\x1b[0m");
    }

    Some(bar)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rainbow_gradient_color_at_zero() {
        let (r, g, b) = rainbow_gradient_color(0.0);
        assert_eq!(r, 80);
        assert_eq!(g, 200);
        assert_eq!(b, 80);
    }

    #[test]
    fn test_rainbow_gradient_color_at_100() {
        let (r, g, b) = rainbow_gradient_color(100.0);
        assert_eq!(r, 200);
        assert_eq!(g, 100);
        assert_eq!(b, 80);
    }

    #[test]
    fn test_rainbow_gradient_color_clamping() {
        let (r1, g1, b1) = rainbow_gradient_color(-10.0);
        let (r2, g2, b2) = rainbow_gradient_color(0.0);
        assert_eq!((r1, g1, b1), (r2, g2, b2));

        let (r3, g3, b3) = rainbow_gradient_color(110.0);
        let (r4, g4, b4) = rainbow_gradient_color(100.0);
        assert_eq!((r3, g3, b3), (r4, g4, b4));
    }

    #[test]
    fn test_build_progress_bar_zero_width() {
        let params = ProgressBarParams {
            percentage: 50.0,
            width: 0,
            filled_char: '█',
            empty_char: '░',
            backup_char: '▓',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        assert!(build_progress_bar(&params).is_none());
    }

    #[test]
    fn test_build_progress_bar_no_gradient() {
        let params = ProgressBarParams {
            percentage: 50.0,
            width: 10,
            filled_char: '#',
            empty_char: '-',
            backup_char: '!',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // 50% of 10 = 5 filled, 5 empty
        assert_eq!(bar.chars().filter(|&c| c == '#').count(), 5);
        assert_eq!(bar.chars().filter(|&c| c == '-').count(), 5);
    }

    #[test]
    fn test_build_progress_bar_100_percent() {
        let params = ProgressBarParams {
            percentage: 100.0,
            width: 5,
            filled_char: '█',
            empty_char: '░',
            backup_char: '▓',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // At 100% with backup_threshold 85, all cells will be backup chars
        let filled_or_backup = bar.chars().filter(|&c| c == '█' || c == '▓').count();
        assert_eq!(filled_or_backup, 5);
    }

    #[test]
    fn test_build_progress_bar_backup_chars() {
        let params = ProgressBarParams {
            percentage: 100.0,
            width: 10,
            filled_char: '.',
            empty_char: '-',
            backup_char: '!',
            backup_threshold: 50.0, // Low threshold so high percentages trigger backup
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // At 100% with threshold 50%, cells above 50% gradient should be backup
        assert!(bar.contains('!'));
    }

    #[test]
    fn test_clamp_round_to_usize() {
        assert_eq!(clamp_round_to_usize(0.0, 10), 0);
        assert_eq!(clamp_round_to_usize(5.4, 10), 5);
        assert_eq!(clamp_round_to_usize(5.5, 10), 6);
        assert_eq!(clamp_round_to_usize(10.0, 10), 10);
        assert_eq!(clamp_round_to_usize(15.0, 10), 10); // Clamped
        assert_eq!(clamp_round_to_usize(-1.0, 10), 0); // Clamped
    }

    #[test]
    fn test_to_f64() {
        let us_val = to_f64(42usize);
        let u64_val = to_f64(100u64);
        assert!((us_val - 42.0).abs() < f64::EPSILON);
        assert!((u64_val - 100.0).abs() < f64::EPSILON);
    }

    // ===== 新增测试 =====

    #[test]
    fn test_rainbow_gradient_midpoints() {
        // 25% boundary: green → yellow-green transition
        let (r25, _, _) = rainbow_gradient_color(25.0);
        assert_eq!(r25, 150); // exact yellow-green

        // 50% boundary: yellow-green → yellow transition
        let (_, g50, _) = rainbow_gradient_color(50.0);
        assert_eq!(g50, 200); // exact yellow

        // 75% boundary: yellow → orange transition
        let (r75, _, _) = rainbow_gradient_color(75.0);
        assert_eq!(r75, 220); // exact orange
    }

    #[test]
    fn test_build_progress_bar_with_gradient() {
        let params = ProgressBarParams {
            percentage: 50.0,
            width: 10,
            filled_char: '█',
            empty_char: '░',
            backup_char: '▓',
            backup_threshold: 85.0,
            gradient_enabled: true,
            supports_colors: true,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // Should contain ANSI escape sequences for true color
        assert!(bar.contains("\x1b[38;2;"));
        // Should end with reset
        assert!(bar.ends_with("\x1b[0m"));
    }

    #[test]
    fn test_build_progress_bar_0_percent() {
        let params = ProgressBarParams {
            percentage: 0.0,
            width: 5,
            filled_char: '#',
            empty_char: '-',
            backup_char: '!',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        assert_eq!(bar, "-----");
    }

    #[test]
    fn test_build_progress_bar_exactly_50_percent() {
        let params = ProgressBarParams {
            percentage: 50.0,
            width: 4,
            filled_char: '#',
            empty_char: '-',
            backup_char: '!',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // 50% of 4 = 2 filled, 2 empty
        assert_eq!(bar, "##--");
    }

    #[test]
    fn test_build_progress_bar_width_1() {
        let params = ProgressBarParams {
            percentage: 75.0,
            width: 1,
            filled_char: '#',
            empty_char: '-',
            backup_char: '!',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        assert_eq!(bar, "#");
    }

    #[test]
    fn test_build_progress_bar_over_100_percent() {
        let params = ProgressBarParams {
            percentage: 150.0,
            width: 6,
            filled_char: '#',
            empty_char: '-',
            backup_char: '!',
            backup_threshold: 50.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // All cells should be filled (capped at width)
        let filled = bar.chars().filter(|&c| c == '#' || c == '!').count();
        assert_eq!(filled, 6);
    }

    #[test]
    fn test_clamp_round_to_usize_edge() {
        assert_eq!(clamp_round_to_usize(0.4, 10), 0);
        assert_eq!(clamp_round_to_usize(0.5, 10), 1);
        assert_eq!(clamp_round_to_usize(9.5, 10), 10);
    }

    #[test]
    fn test_into_f64_trait() {
        // Ensure IntoF64 is implemented for both usize and u64
        let us: usize = 42;
        let u64v: u64 = 100;
        assert!((us.into_f64() - 42.0).abs() < f64::EPSILON);
        assert!((u64v.into_f64() - 100.0).abs() < f64::EPSILON);
    }

    // ===== 三段式进度条测试 =====

    #[test]
    fn test_three_segment_progress_bar_with_baseline() {
        // baseline 9%, used 29% => 10 char bar: 1 baseline + 2 user + 7 empty
        let params = ProgressBarParams {
            percentage: 29.0,
            width: 10,
            filled_char: '█',
            empty_char: '░',
            backup_char: '▓',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: Some(9.0),
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        let baseline_count = bar.chars().filter(|&c| c == '▓').count();
        let filled_count = bar.chars().filter(|&c| c == '█').count();
        let empty_count = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(baseline_count, 1); // 9% of 10 ≈ 1
        assert_eq!(filled_count, 2); // 29% of 10 ≈ 3 total filled - 1 baseline = 2
        assert_eq!(empty_count, 7);
    }

    #[test]
    fn test_three_segment_progress_bar_no_baseline() {
        // 无基线时行为应与原始一致
        let params = ProgressBarParams {
            percentage: 50.0,
            width: 10,
            filled_char: '█',
            empty_char: '░',
            backup_char: '▓',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: None,
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // backup_char 不应出现（因为无基线且未超过阈值）
        let filled_count = bar.chars().filter(|&c| c == '█').count();
        let empty_count = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(filled_count, 5);
        assert_eq!(empty_count, 5);
    }

    #[test]
    fn test_three_segment_progress_bar_zero_baseline() {
        // 基线为 0 时等同于无基线
        let params = ProgressBarParams {
            percentage: 50.0,
            width: 10,
            filled_char: '█',
            empty_char: '░',
            backup_char: '▓',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: Some(0.0),
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // backup_char 不应出现（基线为 0，未超过 backup 阈值）
        let filled_count = bar.chars().filter(|&c| c == '█').count();
        assert_eq!(filled_count, 5);
    }

    #[test]
    fn test_three_segment_progress_bar_baseline_exceeds_used() {
        // 基线 60% 但 used 只有 40%：全部已填充区域都是基线
        let params = ProgressBarParams {
            percentage: 40.0,
            width: 10,
            filled_char: '█',
            empty_char: '░',
            backup_char: '▓',
            backup_threshold: 85.0,
            gradient_enabled: false,
            supports_colors: false,
            baseline_percentage: Some(60.0),
        };
        let bar = build_progress_bar(&params).unwrap_or_default();
        // 40% of 10 = 4 filled (all baseline), 6 empty
        let baseline_count = bar.chars().filter(|&c| c == '▓').count();
        let empty_count = bar.chars().filter(|&c| c == '░').count();
        assert_eq!(baseline_count, 4);
        assert_eq!(empty_count, 6);
    }
}
