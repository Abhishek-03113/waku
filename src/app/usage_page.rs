//! The settings Usage page: historical token and cost usage across provider
//! transcripts, mirroring T3 Code's usage dashboard — a windowed headline with
//! per-provider share bars, a layered daily chart, a metric strip, a
//! model/day breakdown, and cost quality. Data comes from
//! [`crate::usage_history`], scanned on the background executor; frames read
//! only the snapshot stored on the entity.

use chrono::NaiveDate;
use gpui::{PathBuilder, relative};

use super::*;
use crate::usage_history::{self, PricingStatus, UsageHistory, UsageProvider, WINDOW_OPTIONS};

/// Rendered chart height, matching T3's `h-56` plot.
const CHART_HEIGHT: f32 = 224.0;
/// Sliver above the top gridline so a peak's 2px stroke is not shaved off.
const CHART_PLOT_TOP: f32 = 8.0;
const CHART_TICKS: usize = 4;
/// Width of the y-axis label gutter.
const CHART_GUTTER: f32 = 56.0;
/// A snapshot older than this rescans when the page is next opened.
const USAGE_RESCAN_AFTER: Duration = Duration::from_secs(120);
/// The in-memory rate table is revalidated against its disk TTL this often.
const USAGE_RATES_RELOAD: Duration = Duration::from_secs(3600);

fn provider_kind(provider: UsageProvider) -> ProviderKind {
    match provider {
        UsageProvider::Claude => ProviderKind::Claude,
        UsageProvider::Codex => ProviderKind::Codex,
    }
}

impl Waku {
    /// Switch the settings view to `page`, warming the Usage scan when that
    /// is where the user is heading.
    pub(super) fn open_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        self.settings_page = Some(page);
        if page == SettingsPage::Usage {
            self.ensure_usage_history(false, cx);
        }
        cx.notify();
    }

    /// Start a background transcript scan unless a current-enough snapshot
    /// (or an in-flight scan for the same window) already covers it. `force`
    /// is the refresh button. Results from superseded scans are discarded by
    /// generation, so a window change mid-scan cannot land stale data.
    pub(super) fn ensure_usage_history(&mut self, force: bool, cx: &mut Context<Self>) {
        let window_days = self.usage_window_days;
        let satisfied = self
            .usage_history
            .as_ref()
            .is_some_and(|history| history.window_days == window_days)
            && self
                .usage_history_scanned_at
                .is_some_and(|scanned| scanned.elapsed() < USAGE_RESCAN_AFTER);
        if !force && (satisfied || self.usage_history_pending_for == Some(window_days)) {
            return;
        }
        self.usage_history_pending_for = Some(window_days);
        self.usage_history_generation += 1;
        let generation = self.usage_history_generation;
        let cache = std::sync::Arc::clone(&self.usage_scan_cache);
        let rate_table = std::sync::Arc::clone(&self.usage_rate_table);
        let rates_dir = self.usage_rates_dir.clone();
        cx.spawn(async move |this, cx| {
            let history = cx
                .background_executor()
                .spawn(async move {
                    // The rate table is shared across scans and revalidated
                    // hourly; `load_rate_table` itself serves its disk cache
                    // within TTL, so this rarely touches the network.
                    let rates = {
                        let mut slot = rate_table
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        match slot
                            .as_ref()
                            .filter(|(loaded, _)| loaded.elapsed() < USAGE_RATES_RELOAD)
                        {
                            Some((_, rates)) => rates.clone(),
                            None => {
                                let rates = usage_history::load_rate_table(&rates_dir);
                                *slot = Some((Instant::now(), rates.clone()));
                                rates
                            }
                        }
                    };
                    let mut cache = cache
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner());
                    usage_history::scan(&mut cache, &rates, window_days)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.usage_history_generation != generation {
                    return;
                }
                this.usage_history_pending_for = None;
                this.usage_history_scanned_at = Some(Instant::now());
                // The day axis may have changed length; a stale index would
                // point at the wrong day.
                this.usage_chart_hover = None;
                this.usage_history = Some(history);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn set_usage_window_days(&mut self, days: u32, cx: &mut Context<Self>) {
        if self.usage_window_days == days {
            return;
        }
        self.usage_window_days = days;
        self.ensure_usage_history(false, cx);
        cx.notify();
    }

    pub(super) fn render_usage_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::current(cx);
        let pending = self.usage_history_pending_for.is_some();
        let Some(history) = self.usage_history.as_ref() else {
            return div()
                .mt(px(15.0))
                .py(px(64.0))
                .w_full()
                .flex()
                .justify_center()
                .text_size(px(12.0))
                .text_color(theme.text_secondary)
                .child("Scanning provider transcripts…")
                .into_any_element();
        };

        let mut page = div()
            .flex()
            .flex_col()
            .child(self.render_usage_header(history, pending, &theme, cx));

        if !history.errors.is_empty() || history.pricing == PricingStatus::Unavailable {
            page = page.child(usage_notices(history, &theme));
        }

        page.child(
            div()
                .mt(px(20.0))
                .flex()
                .items_start()
                .gap(px(28.0))
                .child(self.render_usage_summary(history, &theme, cx))
                .child(self.render_usage_chart_column(history, &theme, cx)),
        )
        .child(usage_metric_strip(history, &theme))
        .child(
            div()
                .mt(px(24.0))
                .flex()
                .items_start()
                .gap(px(32.0))
                .child(self.render_usage_breakdown(history, &theme, cx))
                .child(usage_quality_panel(history, &theme)),
        )
        .child(
            // What the numbers above are built from, so the totals are
            // auditable at a glance.
            div()
                .mt(px(18.0))
                .text_size(px(9.5))
                .text_color(theme.text_ghost)
                .child(SharedString::from(format!(
                    "Scanned {} transcripts ({} outside the window) · {} usage records · {:.1}s",
                    format_count(history.scanned_files as u64),
                    format_count(history.skipped_files as u64),
                    format_count(history.records),
                    history.scan_duration.as_secs_f64(),
                ))),
        )
        .into_any_element()
    }

    /// The range caption plus the window selector and refresh control.
    fn render_usage_header(
        &self,
        history: &UsageHistory,
        pending: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut window_options = div()
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .overflow_hidden();
        for days in WINDOW_OPTIONS {
            let selected = self.usage_window_days == days;
            window_options = window_options.child(
                div()
                    .id(SharedString::from(format!("usage-window-{days}")))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .h(px(26.0))
                    .px(px(11.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(10.5))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| element.bg(theme.overlay))
                    .when(!selected, |element| {
                        element.hover(|element| element.text_color(theme.text))
                    })
                    .child(SharedString::from(format!("{days} days")))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_usage_window_days(days, cx);
                    })),
            );
        }

        let refresh = div()
            .id("usage-refresh")
            .tab_index(0)
            .focus_visible(|style| style.border_color(theme.accent))
            .h(px(26.0))
            .px(px(8.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .items_center()
            .cursor_default()
            .opacity(if pending { 0.6 } else { 1.0 })
            .hover(|element| element.bg(theme.overlay))
            .tooltip(Tooltip::text(if pending {
                "Scanning provider transcripts…"
            } else {
                "Rescan provider transcripts"
            }))
            .child(icon("icons/rotate-cw.svg", 12.0, theme.text_tertiary))
            .on_click(cx.listener(|this, _, _, cx| {
                this.ensure_usage_history(true, cx);
            }));

        div()
            .mt(px(6.0))
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px(12.5))
                    .text_color(theme.text_secondary)
                    .child(SharedString::from(format!(
                        "{} to {}",
                        format_day_short(history.since_day),
                        format_day_short(history.until_day)
                    ))),
            )
            .child(window_options)
            .child(refresh)
    }

    /// The headline figure in the active metric plus one share bar per
    /// provider. The summary follows the chart toggle so the headline and the
    /// series always read the same units.
    fn render_usage_summary(
        &self,
        history: &UsageHistory,
        theme: &Theme,
        _cx: &mut Context<Self>,
    ) -> Div {
        let metric = self.usage_metric;
        let headline = match metric {
            UsageMetric::Cost => format!("{}*", format_usd(history.cost_usd)),
            UsageMetric::Tokens => format_tokens_compact(history.total_tokens as f64),
        };
        let caption = match metric {
            UsageMetric::Cost => "* if billed at full API rate".to_owned(),
            UsageMetric::Tokens => format!(
                "Input, cache reads and output across {} sessions.",
                format_count(history.sessions)
            ),
        };

        let mut column = div()
            .w(px(300.0))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(18.0))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(theme.text_tertiary)
                            .child(match metric {
                                UsageMetric::Cost => "RAW TOKEN COST",
                                UsageMetric::Tokens => "PROCESSED TOKENS",
                            }),
                    )
                    .child(
                        div()
                            .text_size(px(30.0))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(SharedString::from(headline)),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(caption)),
                    ),
            );

        // Ranked by whatever the toggle is showing, so the bars always
        // descend.
        let mut providers = history.providers.clone();
        if metric == UsageMetric::Tokens {
            providers.sort_by(|a, b| b.total_tokens.cmp(&a.total_tokens));
        }
        for provider in &providers {
            let kind = provider_kind(provider.provider);
            let color = provider_color(theme, kind);
            let share = match metric {
                UsageMetric::Cost => provider.cost_share,
                UsageMetric::Tokens => provider.token_share,
            };
            let value = match metric {
                UsageMetric::Cost => format_usd(provider.cost_usd),
                UsageMetric::Tokens => format_tokens_compact(provider.total_tokens as f64),
            };
            let detail = match metric {
                UsageMetric::Cost => format!(
                    "{} of cost · {} tokens",
                    format_percent(share),
                    format_tokens_compact(provider.total_tokens as f64)
                ),
                UsageMetric::Tokens => format!(
                    "{} of tokens · {}",
                    format_percent(share),
                    format_usd(provider.cost_usd)
                ),
            };
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(8.0))
                            .child(icon(provider_icon(kind), 14.0, color))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .text_color(theme.text)
                                    .child(provider.provider.label()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .text_color(theme.text)
                                    .child(SharedString::from(value)),
                            ),
                    )
                    .child(
                        div()
                            .h(px(4.0))
                            .w_full()
                            .rounded_full()
                            .bg(theme.overlay_strong)
                            .child(
                                div()
                                    .h_full()
                                    .w(relative((share as f32).clamp(0.0, 1.0)))
                                    .rounded_full()
                                    .bg(color),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.text_tertiary)
                            .child(SharedString::from(detail)),
                    ),
            );
        }
        if history.providers.is_empty() {
            column = column.child(
                div()
                    .text_size(px(11.5))
                    .text_color(theme.text_tertiary)
                    .child("No activity in this window."),
            );
        }
        column
    }

    /// The chart header (title, metric toggle, legend), the layered daily
    /// chart, and its x-axis labels.
    fn render_usage_chart_column(
        &self,
        history: &UsageHistory,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let metric = self.usage_metric;
        let mut toggle = div()
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .overflow_hidden();
        for (option, label) in [(UsageMetric::Cost, "COST"), (UsageMetric::Tokens, "TOKENS")] {
            let selected = metric == option;
            toggle = toggle.child(
                div()
                    .id(SharedString::from(format!("usage-metric-{label}")))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .h(px(22.0))
                    .px(px(9.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(9.0))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| element.bg(theme.overlay))
                    .when(!selected, |element| {
                        element.hover(|element| element.text_color(theme.text))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.usage_metric != option {
                            this.usage_metric = option;
                            cx.notify();
                        }
                    })),
            );
        }

        let mut legend = div().flex().items_center().gap(px(14.0));
        for provider in UsageProvider::ALL {
            let kind = provider_kind(provider);
            legend = legend.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(5.0))
                    .child(icon(provider_icon(kind), 12.0, provider_color(theme, kind)))
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme.text_secondary)
                            .child(provider.label()),
                    ),
            );
        }

        let days = usage_history::enumerate_days(history.since_day, history.until_day);
        div()
            .flex_1()
            .min_w(px(320.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(match metric {
                                UsageMetric::Cost => "Daily cost",
                                UsageMetric::Tokens => "Daily processed tokens",
                            }),
                    )
                    .child(toggle)
                    .child(legend),
            )
            .child(self.render_usage_chart(history, &days, theme, cx))
            .child(
                div()
                    .pl(px(CHART_GUTTER + 8.0))
                    .flex()
                    .justify_between()
                    .text_size(px(9.5))
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(
                        days.first()
                            .copied()
                            .map(format_day_short)
                            .unwrap_or_default(),
                    ))
                    .child(SharedString::from(
                        days.get(days.len() / 2)
                            .copied()
                            .map(format_day_short)
                            .unwrap_or_default(),
                    ))
                    .child(SharedString::from(
                        days.last()
                            .copied()
                            .map(format_day_short)
                            .unwrap_or_default(),
                    )),
            )
    }

    /// The plot: y-axis gutter, layered per-provider curves, and the hover
    /// readout. Values are absolute, not cumulative — the series are layered
    /// from a shared zero baseline rather than stacked, because a stacked
    /// chart puts whichever provider is drawn last permanently above the
    /// other, which reads as "that one is bigger" even on days it is not.
    fn render_usage_chart(
        &self,
        history: &UsageHistory,
        days: &[NaiveDate],
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let metric = self.usage_metric;
        let day_count = days.len();
        // One column per day, per provider in ALL order. The chart paths and
        // the hover readout both consume this, so the number under the cursor
        // is by construction the number that was plotted.
        let series: Vec<[f64; 2]> = days
            .iter()
            .map(|day| {
                let slice = history.day(*day);
                let value = |provider: UsageProvider| {
                    slice
                        .map(|slice| {
                            let entry = slice.by_provider[provider.index()];
                            match metric {
                                UsageMetric::Cost => entry.cost_usd,
                                UsageMetric::Tokens => entry.total_tokens as f64,
                            }
                        })
                        .unwrap_or(0.0)
                };
                [value(UsageProvider::Claude), value(UsageProvider::Codex)]
            })
            .collect();
        // The scale tops out at the largest single provider-day, not the
        // largest sum: layered series each measure from zero, so a combined
        // peak would leave the plot permanently half empty.
        let peak = series
            .iter()
            .flat_map(|bands| bands.iter().copied())
            .fold(0.0_f64, f64::max);
        let (scale_max, ticks) = nice_scale(peak, CHART_TICKS);
        let format_value = move |value: f64| match metric {
            UsageMetric::Cost => format_usd(value),
            UsageMetric::Tokens => format_tokens_compact(value),
        };
        // Fraction of the plot height for a value, shared by the canvas, the
        // gutter labels, and nothing else.
        let to_fraction = move |value: f64| -> f32 {
            if scale_max <= 0.0 {
                1.0
            } else {
                1.0 - (value / scale_max) as f32 * (1.0 - CHART_PLOT_TOP / CHART_HEIGHT)
            }
        };

        let mut gutter = div()
            .relative()
            .w(px(CHART_GUTTER))
            .h(px(CHART_HEIGHT))
            .flex_none();
        for tick in &ticks {
            gutter = gutter.child(
                div()
                    .absolute()
                    .right(px(0.0))
                    .top(px((to_fraction(*tick) * CHART_HEIGHT - 7.0).max(0.0)))
                    .text_size(px(9.5))
                    .text_color(theme.text_tertiary)
                    .child(SharedString::from(if *tick == 0.0 {
                        "0".to_owned()
                    } else {
                        format_value(*tick)
                    })),
            );
        }

        let hover = self.usage_chart_hover.filter(|index| *index < day_count);
        let colors = [
            provider_color(theme, ProviderKind::Claude),
            provider_color(theme, ProviderKind::Codex),
        ];
        let bounds_cell = self.usage_chart_bounds.clone();
        let paint_series = series.clone();
        let paint_ticks = ticks.clone();
        let grid_color = theme.border;
        let hover_color = theme.text_ghost;
        let plot_canvas = canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                bounds_cell.set(Some(bounds));
                let width = f32::from(bounds.size.width);
                let height = f32::from(bounds.size.height);
                let to_y = |value: f64| bounds.origin.y + px(to_fraction(value) * height);
                for tick in &paint_ticks {
                    window.paint_quad(fill(
                        gpui::Bounds::new(
                            point(bounds.origin.x, to_y(*tick)),
                            gpui::size(bounds.size.width, px(1.0)),
                        ),
                        grid_color,
                    ));
                }
                if paint_series.is_empty() {
                    return;
                }

                let step = if paint_series.len() <= 1 {
                    0.0
                } else {
                    width / (paint_series.len() - 1) as f32
                };
                let mut layers: Vec<(usize, f64)> = (0..colors.len())
                    .map(|provider| {
                        (
                            provider,
                            paint_series
                                .iter()
                                .map(|bands| bands[provider])
                                .sum::<f64>(),
                        )
                    })
                    .collect();
                // Paint the heavier series' fill first so the lighter one is
                // never buried under it; the strokes are drawn in a second
                // pass regardless, so neither can be hidden.
                layers.sort_by(|a, b| b.1.total_cmp(&a.1));

                let curves: Vec<(usize, Vec<CurveSegment>)> = layers
                    .iter()
                    .map(|(provider, _)| {
                        let points: Vec<(f32, f32)> = paint_series
                            .iter()
                            .enumerate()
                            .map(|(index, bands)| {
                                (
                                    f32::from(bounds.origin.x) + index as f32 * step,
                                    f32::from(to_y(bands[*provider])),
                                )
                            })
                            .collect();
                        (*provider, smooth_curve(&points))
                    })
                    .collect();

                let bottom = bounds.origin.y + bounds.size.height;
                for (provider, segments) in &curves {
                    let Some(first) = segments.first() else {
                        continue;
                    };
                    let mut area = PathBuilder::fill();
                    area.move_to(point(px(first.from.0), px(first.from.1)));
                    for segment in segments {
                        area.cubic_bezier_to(
                            point(px(segment.to.0), px(segment.to.1)),
                            point(px(segment.c1.0), px(segment.c1.1)),
                            point(px(segment.c2.0), px(segment.c2.1)),
                        );
                    }
                    area.line_to(point(bounds.origin.x + bounds.size.width, bottom));
                    area.line_to(point(bounds.origin.x, bottom));
                    area.close();
                    if let Ok(path) = area.build() {
                        window.paint_path(path, colors[*provider].opacity(0.12));
                    }
                }
                for (provider, segments) in &curves {
                    let Some(first) = segments.first() else {
                        continue;
                    };
                    let mut line = PathBuilder::stroke(px(2.0));
                    line.move_to(point(px(first.from.0), px(first.from.1)));
                    for segment in segments {
                        line.cubic_bezier_to(
                            point(px(segment.to.0), px(segment.to.1)),
                            point(px(segment.c1.0), px(segment.c1.1)),
                            point(px(segment.c2.0), px(segment.c2.1)),
                        );
                    }
                    if let Ok(path) = line.build() {
                        window.paint_path(path, colors[*provider]);
                    }
                }

                if let Some(index) = hover {
                    let x = bounds.origin.x + px(index as f32 * step);
                    window.paint_quad(fill(
                        gpui::Bounds::new(
                            point(x, bounds.origin.y + px(CHART_PLOT_TOP)),
                            gpui::size(px(1.0), bounds.size.height - px(CHART_PLOT_TOP)),
                        ),
                        hover_color,
                    ));
                }
            },
        );

        let plot = div()
            .id("usage-chart-plot")
            .relative()
            .flex_1()
            .min_w(px(0.0))
            .h(px(CHART_HEIGHT))
            .tab_index(0)
            .focus_visible(|style| style.border_1().border_color(theme.accent))
            .on_mouse_move(cx.listener(move |this, event: &MouseMoveEvent, _, cx| {
                let Some(bounds) = this.usage_chart_bounds.get() else {
                    return;
                };
                if day_count == 0 || f32::from(bounds.size.width) <= 0.0 {
                    return;
                }
                let fraction =
                    ((event.position.x - bounds.origin.x) / bounds.size.width).clamp(0.0, 1.0);
                let index = ((fraction * day_count.saturating_sub(1) as f32).round() as usize)
                    .min(day_count - 1);
                if this.usage_chart_hover != Some(index) {
                    this.usage_chart_hover = Some(index);
                    cx.notify();
                }
            }))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !hovered && this.usage_chart_hover.is_some() {
                    this.usage_chart_hover = None;
                    cx.notify();
                }
            }))
            // The hover readout is also keyboard reachable: focus the plot
            // and step days with the arrows.
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if day_count == 0 {
                    return;
                }
                let last = day_count - 1;
                let next = match event.keystroke.key.as_str() {
                    "left" => Some(
                        this.usage_chart_hover
                            .map_or(last, |index| index.saturating_sub(1)),
                    ),
                    "right" => Some(
                        this.usage_chart_hover
                            .map_or(0, |index| (index + 1).min(last)),
                    ),
                    "home" => Some(0),
                    "end" => Some(last),
                    "escape" if this.usage_chart_hover.is_some() => None,
                    _ => return,
                };
                cx.stop_propagation();
                if this.usage_chart_hover != next {
                    this.usage_chart_hover = next;
                    cx.notify();
                }
            }))
            .child(plot_canvas.size_full())
            .when_some(
                hover.and_then(|index| days.get(index).map(|day| (index, *day))),
                |element, (index, day)| {
                    element.child(usage_chart_readout(
                        history,
                        day,
                        if day_count <= 1 {
                            0.0
                        } else {
                            index as f32 / (day_count - 1) as f32
                        },
                        metric,
                        theme,
                    ))
                },
            );

        div().flex().gap(px(8.0)).child(gutter).child(plot)
    }

    /// The breakdown table with its model/day toggle.
    fn render_usage_breakdown(
        &self,
        history: &UsageHistory,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let breakdown = self.usage_breakdown;
        let mut toggle = div()
            .rounded(px(7.0))
            .border_1()
            .border_color(theme.border_strong)
            .flex()
            .overflow_hidden();
        for (option, label) in [
            (UsageBreakdown::Model, "MODEL"),
            (UsageBreakdown::Day, "DAY"),
        ] {
            let selected = breakdown == option;
            toggle = toggle.child(
                div()
                    .id(SharedString::from(format!("usage-breakdown-{label}")))
                    .tab_index(0)
                    .focus_visible(|style| style.border_1().border_color(theme.accent))
                    .h(px(22.0))
                    .px(px(9.0))
                    .flex()
                    .items_center()
                    .cursor_default()
                    .text_size(px(9.0))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |element| element.bg(theme.overlay))
                    .when(!selected, |element| {
                        element.hover(|element| element.text_color(theme.text))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if this.usage_breakdown != option {
                            this.usage_breakdown = option;
                            cx.notify();
                        }
                    })),
            );
        }

        div()
            .flex_1()
            .min_w(px(0.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.5))
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child("Breakdown"),
                    )
                    .child(toggle),
            )
            .child(match breakdown {
                UsageBreakdown::Model => usage_model_table(history, theme),
                UsageBreakdown::Day => usage_day_table(history, theme),
            })
    }
}

/* ------------------------------------------------------------------------- */
/* Stateless sections                                                        */
/* ------------------------------------------------------------------------- */

/// Says plainly when the totals are incomplete: an unreadable transcript
/// directory, or no rate table to price against.
fn usage_notices(history: &UsageHistory, theme: &Theme) -> Div {
    let mut notice = div()
        .mt(px(14.0))
        .px(px(12.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border)
        .flex()
        .flex_col()
        .gap(px(3.0))
        .text_size(px(10.5))
        .text_color(theme.text_tertiary);
    for error in &history.errors {
        notice = notice.child(SharedString::from(error.clone()));
    }
    if history.pricing == PricingStatus::Unavailable {
        notice = notice.child(
            "Model rates are unavailable, so costs read as unpriced until the rate table loads.",
        );
    }
    notice
}

/// One day's per-provider values under the cursor, anchored to the hovered
/// column and flipped near the right edge so it stays inside the plot.
fn usage_chart_readout(
    history: &UsageHistory,
    day: NaiveDate,
    fraction: f32,
    metric: UsageMetric,
    theme: &Theme,
) -> Div {
    let slice = history.day(day);
    let value = |provider: UsageProvider| {
        slice
            .map(|slice| {
                let entry = slice.by_provider[provider.index()];
                match metric {
                    UsageMetric::Cost => entry.cost_usd,
                    UsageMetric::Tokens => entry.total_tokens as f64,
                }
            })
            .unwrap_or(0.0)
    };
    let format_value = |value: f64| match metric {
        UsageMetric::Cost => format_usd(value),
        UsageMetric::Tokens => format_tokens_compact(value),
    };

    let mut readout = div()
        .absolute()
        .top(px(0.0))
        .when(fraction <= 0.6, |element| element.left(relative(fraction)))
        .when(fraction > 0.6, |element| {
            element.right(relative(1.0 - fraction))
        })
        .min_w(px(150.0))
        .px(px(9.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(theme.border_strong)
        .bg(theme.raised)
        .shadow_md()
        .flex()
        .flex_col()
        .gap(px(3.0))
        .text_size(px(10.5))
        .child(
            div()
                .text_color(theme.text_tertiary)
                .child(SharedString::from(format_day_short(day))),
        );
    let mut total = 0.0;
    for provider in UsageProvider::ALL {
        let kind = provider_kind(provider);
        let amount = value(provider);
        total += amount;
        readout = readout.child(
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .child(icon(provider_icon(kind), 11.0, provider_color(theme, kind)))
                .child(
                    div()
                        .flex_1()
                        .text_color(theme.text_secondary)
                        .child(provider.label()),
                )
                .child(
                    div()
                        .text_color(theme.text)
                        .child(SharedString::from(format_value(amount))),
                ),
        );
    }
    readout.child(
        div()
            .mt(px(2.0))
            .pt(px(4.0))
            .border_t_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .flex_1()
                    .text_color(theme.text_secondary)
                    .child("Total"),
            )
            .child(
                div()
                    .text_color(theme.text)
                    .child(SharedString::from(format_value(total))),
            ),
    )
}

/// The five-figure strip under the chart: token mix and cache economics.
fn usage_metric_strip(history: &UsageHistory, theme: &Theme) -> Div {
    let active_days = history
        .daily
        .iter()
        .filter(|day| day.total_tokens > 0)
        .count();
    let daily_average = if active_days == 0 {
        0.0
    } else {
        history.total_tokens as f64 / active_days as f64
    };
    let observed_input = history.totals.uncached_input + history.totals.cached_input;
    let cached_share = if observed_input == 0 {
        0.0
    } else {
        history.totals.cached_input as f64 / observed_input as f64
    };
    let savings_detail = if history.cost_usd > 0.0 {
        format!(
            "{:.1}x the raw token cost",
            history.quality.cache_savings_usd / history.cost_usd
        )
    } else {
        "vs full input rates".to_owned()
    };

    let tiles: [(&str, String, String); 5] = [
        (
            "Processed tokens",
            format_tokens_compact(history.total_tokens as f64),
            format!("{} per active day", format_tokens_compact(daily_average)),
        ),
        (
            "Cached input",
            format_tokens_compact(history.totals.cached_input as f64),
            format!("{} of observed input", format_percent(cached_share)),
        ),
        (
            "Uncached input",
            format_tokens_compact(history.totals.uncached_input as f64),
            format!(
                "{} cache writes",
                format_tokens_compact(history.totals.cache_creation as f64)
            ),
        ),
        (
            "Output",
            format_tokens_compact(history.totals.output as f64),
            format!(
                "includes {} reasoning",
                format_tokens_compact(history.totals.reasoning as f64)
            ),
        ),
        (
            "Cache savings",
            format_usd(history.quality.cache_savings_usd),
            savings_detail,
        ),
    ];

    let mut strip = div()
        .mt(px(24.0))
        .border_t_1()
        .border_b_1()
        .border_color(theme.border)
        .flex();
    for (index, (label, value, detail)) in tiles.into_iter().enumerate() {
        strip = strip.child(
            div()
                .flex_1()
                .min_w_0()
                .px(px(14.0))
                .py(px(11.0))
                .when(index > 0, |element| {
                    element.border_l_1().border_color(theme.border)
                })
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(theme.text_tertiary)
                        .truncate()
                        .child(label),
                )
                .child(
                    div()
                        .text_size(px(15.0))
                        .text_color(theme.text)
                        .truncate()
                        .child(SharedString::from(value)),
                )
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(theme.text_tertiary)
                        .truncate()
                        .child(SharedString::from(detail)),
                ),
        );
    }
    strip
}

fn usage_table_empty_row(theme: &Theme) -> Div {
    div()
        .py(px(24.0))
        .flex()
        .justify_center()
        .text_size(px(11.5))
        .text_color(theme.text_tertiary)
        .child("No activity in this window.")
}

/// Right-aligned numeric cell of fixed width.
fn usage_cell(width: f32, text: String, color: Hsla) -> Div {
    div()
        .w(px(width))
        .flex_none()
        .flex()
        .justify_end()
        .text_color(color)
        .child(SharedString::from(text))
}

/// Per-model costs, largest first.
fn usage_model_table(history: &UsageHistory, theme: &Theme) -> Div {
    let mut table = div().flex().flex_col().text_size(px(11.5)).child(
        div()
            .pb(px(7.0))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(12.0))
            .text_size(px(10.5))
            .text_color(theme.text_tertiary)
            .child(div().flex_1().min_w_0().child("Model"))
            .child(usage_cell(84.0, "Cost".to_owned(), theme.text_tertiary))
            .child(usage_cell(64.0, "Share".to_owned(), theme.text_tertiary))
            .child(usage_cell(84.0, "Tokens".to_owned(), theme.text_tertiary)),
    );
    if history.models.is_empty() {
        return table.child(usage_table_empty_row(theme));
    }
    for model in &history.models {
        let kind = provider_kind(model.provider);
        table = table.child(
            div()
                .py(px(8.0))
                .border_b_1()
                .border_color(theme.border)
                .flex()
                .items_center()
                .gap(px(12.0))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap(px(7.0))
                        .child(icon(provider_icon(kind), 12.0, provider_color(theme, kind)))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text)
                                .child(SharedString::from(model.model.clone())),
                        ),
                )
                .child(usage_cell(84.0, format_usd(model.cost_usd), theme.text))
                .child(usage_cell(
                    64.0,
                    format_percent(model.cost_share),
                    theme.text_tertiary,
                ))
                .child(usage_cell(
                    84.0,
                    format_tokens_compact(model.total_tokens as f64),
                    theme.text_tertiary,
                )),
        );
    }
    table
}

/// The most recent active days, newest first, with per-provider cost columns.
fn usage_day_table(history: &UsageHistory, theme: &Theme) -> Div {
    let mut header = div()
        .pb(px(7.0))
        .border_b_1()
        .border_color(theme.border)
        .flex()
        .items_center()
        .gap(px(12.0))
        .text_size(px(10.5))
        .text_color(theme.text_tertiary)
        .child(div().flex_1().min_w_0().child("Day"));
    for provider in UsageProvider::ALL {
        header = header.child(usage_cell(
            84.0,
            provider.label().to_owned(),
            theme.text_tertiary,
        ));
    }
    header = header
        .child(usage_cell(84.0, "Total".to_owned(), theme.text_tertiary))
        .child(usage_cell(84.0, "Tokens".to_owned(), theme.text_tertiary));

    let mut table = div().flex().flex_col().text_size(px(11.5)).child(header);
    if history.daily.is_empty() {
        return table.child(usage_table_empty_row(theme));
    }
    for day in history.daily.iter().rev().take(8) {
        let mut row = div()
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(12.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(theme.text)
                    .child(SharedString::from(format_day_short(day.day))),
            );
        for provider in UsageProvider::ALL {
            row = row.child(usage_cell(
                84.0,
                format_usd(day.by_provider[provider.index()].cost_usd),
                theme.text_tertiary,
            ));
        }
        table = table.child(
            row.child(usage_cell(84.0, format_usd(day.cost_usd), theme.text))
                .child(usage_cell(
                    84.0,
                    format_tokens_compact(day.total_tokens as f64),
                    theme.text_tertiary,
                )),
        );
    }
    table
}

/// How much of the window's cost is provider-reported, table-priced, or
/// unpriced — the reader's confidence in the headline number.
fn usage_quality_panel(history: &UsageHistory, theme: &Theme) -> Div {
    let row = |label: &'static str, value: String| {
        div()
            .py(px(8.0))
            .border_b_1()
            .border_color(theme.border)
            .flex()
            .items_center()
            .gap(px(12.0))
            .text_size(px(11.5))
            .child(div().flex_1().text_color(theme.text_secondary).child(label))
            .child(
                div()
                    .text_color(theme.text)
                    .child(SharedString::from(value)),
            )
    };
    div()
        .w(px(240.0))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(
            div()
                .text_size(px(12.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme.text)
                .child("Cost quality"),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(row(
                    "Provider reported",
                    format_percent(history.quality.provider_reported_share),
                ))
                .child(row(
                    "Model priced",
                    format_percent(history.quality.model_priced_share),
                ))
                .child(row(
                    "Unpriced",
                    format_percent(history.quality.unpriced_share),
                ))
                .child(row(
                    "Cache savings",
                    format_usd(history.quality.cache_savings_usd),
                )),
        )
}

/* ------------------------------------------------------------------------- */
/* Chart math                                                                */
/* ------------------------------------------------------------------------- */

/// One cubic segment of a smoothed series boundary, in window pixels.
struct CurveSegment {
    from: (f32, f32),
    c1: (f32, f32),
    c2: (f32, f32),
    to: (f32, f32),
}

/// Monotone cubic tangents (Fritsch–Carlson). Plain cubic smoothing
/// overshoots on spiky daily data and would dip the area below zero between
/// points, which reads as negative spend; this variant is shape-preserving,
/// so a smoothed series never leaves the range of its samples.
fn monotone_tangents(points: &[(f32, f32)]) -> Vec<f32> {
    let count = points.len();
    if count < 2 {
        return vec![0.0];
    }
    let mut slopes = Vec::with_capacity(count - 1);
    for index in 0..count - 1 {
        let dx = points[index + 1].0 - points[index].0;
        let dy = points[index + 1].1 - points[index].1;
        slopes.push(if dx == 0.0 { 0.0 } else { dy / dx });
    }

    let mut tangents = vec![0.0; count];
    tangents[0] = slopes[0];
    tangents[count - 1] = slopes[count - 2];
    for index in 1..count - 1 {
        let previous = slopes[index - 1];
        let next = slopes[index];
        tangents[index] = if previous * next <= 0.0 {
            0.0
        } else {
            (previous + next) / 2.0
        };
    }

    for index in 0..count - 1 {
        let slope = slopes[index];
        if slope == 0.0 {
            tangents[index] = 0.0;
            tangents[index + 1] = 0.0;
            continue;
        }
        let a = tangents[index] / slope;
        let b = tangents[index + 1] / slope;
        let magnitude = a * a + b * b;
        if magnitude > 9.0 {
            let scale = 3.0 / magnitude.sqrt();
            tangents[index] = scale * a * slope;
            tangents[index + 1] = scale * b * slope;
        }
    }
    tangents
}

/// Smoothed polyline through `points`, as explicit cubic control points.
fn smooth_curve(points: &[(f32, f32)]) -> Vec<CurveSegment> {
    if points.len() < 2 {
        return Vec::new();
    }
    let tangents = monotone_tangents(points);
    let mut segments = Vec::with_capacity(points.len() - 1);
    for index in 0..points.len() - 1 {
        let from = points[index];
        let to = points[index + 1];
        let dx = to.0 - from.0;
        segments.push(CurveSegment {
            from,
            c1: (from.0 + dx / 3.0, from.1 + tangents[index] * dx / 3.0),
            c2: (to.0 - dx / 3.0, to.1 - tangents[index + 1] * dx / 3.0),
            to,
        });
    }
    segments
}

/// A scale whose maximum is a readable 1/2/5 × 10ⁿ step at or above the
/// peak. Rounding the maximum *up* is the point: stopping at the last step
/// below the peak leaves the tallest day drawn past the top of the plot,
/// where it is clipped.
fn nice_scale(peak: f64, count: usize) -> (f64, Vec<f64>) {
    if peak <= 0.0 {
        return (0.0, vec![0.0]);
    }
    let raw_step = peak / count as f64;
    let magnitude = 10.0_f64.powf(raw_step.log10().floor());
    let normalized = raw_step / magnitude;
    let step = if normalized > 5.0 {
        10.0
    } else if normalized > 2.0 {
        5.0
    } else if normalized > 1.0 {
        2.0
    } else {
        1.0
    } * magnitude;
    let max = (peak / step).ceil() * step;
    let mut ticks = Vec::new();
    let mut value = 0.0;
    while value <= max + step * 1e-6 {
        ticks.push(value);
        value += step;
    }
    (max, ticks)
}

/* ------------------------------------------------------------------------- */
/* Formatting                                                                */
/* ------------------------------------------------------------------------- */

fn group_thousands(number: &str) -> String {
    let (integer, fraction) = match number.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (number, None),
    };
    let grouped = integer
        .as_bytes()
        .rchunks(3)
        .rev()
        .map(|chunk| std::str::from_utf8(chunk).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(",");
    match fraction {
        Some(fraction) => format!("{grouped}.{fraction}"),
        None => grouped,
    }
}

fn format_usd(value: f64) -> String {
    format!("${}", group_thousands(&format!("{:.2}", value.max(0.0))))
}

fn format_count(value: u64) -> String {
    group_thousands(&value.to_string())
}

/// Compacts a token count to three significant figures with a unit suffix, so
/// columns of numbers line up at a glance (`19.9B`, `76.7M`, `804K`).
fn format_tokens_compact(value: f64) -> String {
    let abs = value.abs();
    let (scaled, suffix) = if abs >= 1e12 {
        (value / 1e12, "T")
    } else if abs >= 1e9 {
        (value / 1e9, "B")
    } else if abs >= 1e6 {
        (value / 1e6, "M")
    } else if abs >= 1e3 {
        (value / 1e3, "K")
    } else {
        return format_count(value.round().max(0.0) as u64);
    };
    let digits = if scaled.abs() >= 100.0 {
        0
    } else if scaled.abs() >= 10.0 {
        1
    } else {
        2
    };
    let mut text = format!("{scaled:.digits$}");
    // Trim an all-zero fraction ("1.00" → "1") but keep "1.50".
    if let Some(dot) = text.find('.')
        && text[dot + 1..].bytes().all(|byte| byte == b'0')
    {
        text.truncate(dot);
    }
    format!("{text}{suffix}")
}

fn format_percent(share: f64) -> String {
    format!("{:.1}%", share * 100.0)
}

/// `2026-08-07` → `Aug 7`.
fn format_day_short(day: NaiveDate) -> String {
    day.format("%b %-d").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_counts_compact_to_three_significant_figures() {
        assert_eq!(format_tokens_compact(804_000.0), "804K");
        assert_eq!(format_tokens_compact(76_700_000.0), "76.7M");
        assert_eq!(format_tokens_compact(19_900_000_000.0), "19.9B");
        assert_eq!(format_tokens_compact(950.0), "950");
        assert_eq!(format_tokens_compact(1_000.0), "1K");
        assert_eq!(format_tokens_compact(1_500.0), "1.50K");
    }

    #[test]
    fn currency_groups_thousands() {
        assert_eq!(format_usd(0.0), "$0.00");
        assert_eq!(format_usd(1_234.5), "$1,234.50");
        assert_eq!(format_usd(1_234_567.891), "$1,234,567.89");
    }

    #[test]
    fn nice_scales_round_up_to_readable_steps() {
        let (max, ticks) = nice_scale(97.0, 4);
        assert_eq!(max, 100.0);
        assert_eq!(ticks, vec![0.0, 50.0, 100.0]);
        let (max, _) = nice_scale(0.37, 4);
        assert!(max >= 0.37);
        let (max, ticks) = nice_scale(0.0, 4);
        assert_eq!(max, 0.0);
        assert_eq!(ticks, vec![0.0]);
    }

    #[test]
    fn monotone_smoothing_never_overshoots_flat_runs() {
        // A spike between two flat runs: the flat segments must stay flat
        // (zero tangents), which is what keeps the area fill from dipping
        // below zero.
        let points = [
            (0.0, 100.0),
            (10.0, 100.0),
            (20.0, 0.0),
            (30.0, 100.0),
            (40.0, 100.0),
        ];
        let tangents = monotone_tangents(&points);
        assert_eq!(tangents[0], 0.0);
        assert_eq!(tangents[1], 0.0);
        assert_eq!(tangents[3], 0.0);
        assert_eq!(tangents[4], 0.0);
        let segments = smooth_curve(&points);
        assert_eq!(segments.len(), 4);
        for segment in &segments {
            for y in [segment.c1.1, segment.c2.1] {
                assert!(
                    (-0.001..=100.001).contains(&y),
                    "control point left range: {y}"
                );
            }
        }
    }
}
