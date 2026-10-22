use super::*;
use crate::tools::plan::PlanSnapshot;
use crate::tui::history::{
    CellFoldAction, CellFoldActionTarget, ExecCell, ExecSource, HistoryCell, PlanUpdateCell,
    ToolCell, ToolStatus, TranscriptActionOwner, TranscriptFold,
};
use codewhale_localization::Locale;
use codewhale_palette as palette;

impl TranscriptViewCache {
    pub(crate) fn fold_action_target(&self) -> Option<CellFoldActionTarget> {
        self.fold_action_target
    }

    fn streaming_lines_reflattened(&self) -> u64 {
        self.streaming_lines_reflattened
    }

    fn streaming_meta_rows_scanned(&self) -> u64 {
        self.streaming_meta_rows_scanned
    }
}

fn plain_lines(cache: &TranscriptViewCache) -> Vec<String> {
    cache
        .lines()
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect()
}

fn user_cell(content: &str) -> HistoryCell {
    HistoryCell::User {
        content: content.to_string(),
    }
}

fn assistant_cell(content: &str, streaming: bool) -> HistoryCell {
    HistoryCell::Assistant {
        content: content.to_string(),
        streaming,
    }
}

fn reasoning_cell(streaming: bool) -> HistoryCell {
    HistoryCell::Thinking {
        content: (1..=20)
            .map(|line| format!("reasoning line {line:02}"))
            .collect::<Vec<_>>()
            .join("\n"),
        streaming,
        duration_secs: (!streaming).then_some(1.0),
    }
}

fn reasoning_owner(cell_index: usize) -> TranscriptActionOwner {
    TranscriptActionOwner {
        cell_index,
        identity_epoch: 7,
    }
}

#[test]
fn ordinary_fold_keeps_original_owner_through_filter_and_narrow_resize() {
    let body = (1..=12)
        .map(|line| format!("answer line {line:02}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let cells = [assistant_cell(&body, false)];
    let folds = HashMap::from([(7, TranscriptFold::Collapsed)]);
    let owner = reasoning_owner(7);
    let options = TranscriptRenderOptions {
        low_motion: true,
        ..Default::default()
    };
    let mut cache = TranscriptViewCache::new();
    for width in 1..=40 {
        cache.ensure_split(&[&cells], &[1], width, options, &folds, Some(&[7]), None);
        let rows = cache.total_lines();
        assert!(rows > 0);
        assert!(cache.lines()[0].width() <= usize::from(width));
        cache.retarget(Some(owner), Some(&[7]));
        assert_eq!(cache.total_lines(), rows, "hint never changes geometry");
        assert_eq!(
            cache.fold_action_target(),
            Some(CellFoldActionTarget {
                owner,
                action: CellFoldAction::Expand
            })
        );
        assert!(cache.lines()[0].width() <= usize::from(width));
        assert_eq!(
            cache.line_meta()[0].copy_prefix_width(),
            cache.lines()[0].width()
        );
        cache.retarget(Some(reasoning_owner(0)), Some(&[7]));
        assert!(
            cache.fold_action_target().is_none(),
            "filtered index is not original identity"
        );
    }
}

#[test]
fn folded_streaming_answer_preserves_links_and_restores_latest_body() {
    let content = format!(
        "[reference](https://example.com/reference)\n\n{}",
        (1..=12)
            .map(|line| format!("paragraph {line:02}"))
            .collect::<Vec<_>>()
            .join("\n\n")
    );
    let mut cells = [assistant_cell(&content, true)];
    let options = TranscriptRenderOptions {
        low_motion: true,
        ..Default::default()
    };
    let mut cache = TranscriptViewCache::new();
    let owner = Some(reasoning_owner(0));
    let mut folds = HashMap::from([(0, TranscriptFold::Collapsed)]);
    cache.ensure_split(&[&cells], &[1], 80, options, &folds, None, owner);
    assert!(plain_lines(&cache).join("\n").contains("Space:expand"));
    assert!(!plain_lines(&cache).join("\n").contains("paragraph 12"));
    assert!(cache.line_links[0].is_empty());
    assert!(
        cache
            .line_links
            .iter()
            .skip(1)
            .any(|links| !links.is_empty()),
        "preview body retains its link"
    );
    let before = (cache.cells_rendered, cache.streaming_lines_reflattened());
    cache.ensure_split(&[&cells], &[1], 80, options, &folds, None, owner);
    assert_eq!(
        (cache.cells_rendered, cache.streaming_lines_reflattened()),
        before,
        "settled folded frame does no work"
    );

    let HistoryCell::Assistant { content, .. } = &mut cells[0] else {
        unreachable!()
    };
    content.push_str("\n\nlatest streamed paragraph");
    cache.ensure_split(&[&cells], &[2], 80, options, &folds, None, owner);
    assert!(
        !plain_lines(&cache)
            .join("\n")
            .contains("latest streamed paragraph")
    );
    assert_eq!(
        cache.fold_action_target().unwrap().action,
        CellFoldAction::Expand
    );
    folds.insert(0, TranscriptFold::Expanded);
    cache.ensure_split(&[&cells], &[2], 80, options, &folds, None, owner);
    assert!(
        plain_lines(&cache)
            .join("\n")
            .contains("latest streamed paragraph")
    );
    let mut cold = TranscriptViewCache::new();
    cold.ensure_split(&[&cells], &[2], 80, options, &folds, None, owner);
    assert_same_flat_output(&cache, &cold);

    let HistoryCell::Assistant { content, .. } = &mut cells[0] else {
        unreachable!()
    };
    content.push_str("\n\ncontinued after unfolding");
    cache.ensure_split(&[&cells], &[3], 80, options, &folds, None, owner);
    let mut cold = TranscriptViewCache::new();
    cold.ensure_split(&[&cells], &[3], 80, options, &folds, None, owner);
    assert_same_flat_output(&cache, &cold);
    assert!(
        plain_lines(&cache)
            .join("\n")
            .contains("continued after unfolding")
    );
}

fn exec_tool_cell_with_output(command: &str, output: String) -> HistoryCell {
    // A failed shell cell keeps its full output in the live render, so
    // this fixture proves tool cells do not inherit the prose measure.
    HistoryCell::Tool(ToolCell::Exec(ExecCell {
        command: command.to_string(),
        status: ToolStatus::Failed,
        output: Some(output),
        live_output: None,
        shell_task_id: None,
        owner_agent_id: None,
        owner_agent_name: None,
        started_at: None,
        duration_ms: None,
        stale_elapsed_since_output_ms: None,
        waiting: None,
        source: ExecSource::Assistant,
        description: None,
        interaction: None,
        output_summary: None,
    }))
}

fn exec_tool_cell(command: &str) -> HistoryCell {
    HistoryCell::Tool(ToolCell::Exec(ExecCell {
        command: command.to_string(),
        status: ToolStatus::Running,
        output: None,
        live_output: None,
        shell_task_id: None,
        owner_agent_id: None,
        owner_agent_name: None,
        started_at: None,
        duration_ms: None,
        stale_elapsed_since_output_ms: None,
        waiting: None,
        source: ExecSource::Assistant,
        description: None,
        interaction: None,
        output_summary: None,
    }))
}

fn durable_work_cell() -> HistoryCell {
    HistoryCell::Tool(ToolCell::PlanUpdate(PlanUpdateCell {
        snapshot: PlanSnapshot::default(),
        status: ToolStatus::Running,
    }))
}

fn spacer_rows_after_cell(cache: &TranscriptViewCache, target_cell: usize) -> usize {
    let mut saw_target = false;
    let mut spacer_rows = 0;
    for meta in cache.line_meta() {
        match meta {
            TranscriptLineMeta::CellLine { cell_index, .. } if *cell_index == target_cell => {
                saw_target = true;
                spacer_rows = 0;
            }
            TranscriptLineMeta::Spacer { .. } if saw_target => spacer_rows += 1,
            TranscriptLineMeta::CellLine { .. } if saw_target => break,
            TranscriptLineMeta::Spacer { .. } | TranscriptLineMeta::CellLine { .. } => {}
        }
    }
    spacer_rows
}

#[test]
fn revision_key_short_circuits_the_unchanged_prefix_scan() {
    let cells = vec![
        user_cell("first prompt"),
        assistant_cell("first answer", false),
        user_cell("second prompt"),
    ];
    let revisions = vec![1u64, 2, 3];
    let options = TranscriptRenderOptions::default();

    let mut cache = TranscriptViewCache::new();
    cache.note_revision_key(24);
    cache.ensure(&cells, &revisions, 40, options);
    let after_first = cache.cells_rendered;
    assert!(after_first >= 3);
    // The very first frame is layout-cold (width changes), so it renders
    // everything through the layout path without running the scan.
    assert_eq!(cache.revision_scan_runs, 0);

    // Same key, same revisions, warm layout: a scan here would find nothing,
    // but the key match must skip it entirely — the counter stays put.
    cache.note_revision_key(24);
    cache.ensure(&cells, &revisions, 40, options);
    assert_eq!(cache.cells_rendered, after_first);
    assert_eq!(cache.revision_scan_runs, 0);

    // A changed key re-runs the authoritative scan. The revisions really are
    // identical, so it re-renders nothing — the key accelerates but never
    // invalidates.
    cache.note_revision_key(25);
    cache.ensure(&cells, &revisions, 40, options);
    assert_eq!(cache.cells_rendered, after_first);
    assert_eq!(cache.revision_scan_runs, 1);

    // An unkeyed frame clears key acceptance, so the next keyed frame scans
    // again even though the key value repeats.
    cache.ensure(&cells, &revisions, 40, options);
    assert_eq!(cache.revision_scan_runs, 2);
    cache.note_revision_key(25);
    cache.ensure(&cells, &revisions, 40, options);
    assert_eq!(cache.revision_scan_runs, 3);
    assert_eq!(cache.cells_rendered, after_first);
}

/// The caller-side whole-frame skip is allowed only when every `ensure_iter`
/// input is pinned equal to the accepted frame; each invalidator below must
/// independently force the full path (#6652).
#[test]
fn keyed_skip_available_requires_every_cache_input_to_match() {
    use crate::tui::history::TranscriptFold;
    use std::collections::HashMap;

    let cells = vec![user_cell("prompt"), assistant_cell("answer", false)];
    let revisions = vec![1u64, 2];
    let options = TranscriptRenderOptions::default();
    let mut cache = TranscriptViewCache::new();
    cache.note_revision_key(7);
    cache.ensure(&cells, &revisions, 40, options);

    // Cold until a key is staged against the accepted frame.
    assert!(!cache.keyed_skip_available(40, options, &HashMap::new(), 2, None));
    cache.note_revision_key(7);
    assert!(cache.keyed_skip_available(40, options, &HashMap::new(), 2, None));

    cache.note_revision_key(7);
    assert!(
        !cache.keyed_skip_available(41, options, &HashMap::new(), 2, None),
        "width"
    );
    cache.note_revision_key(7);
    let mut other_options = options;
    other_options.show_thinking = !other_options.show_thinking;
    assert!(
        !cache.keyed_skip_available(40, other_options, &HashMap::new(), 2, None),
        "render options"
    );
    cache.note_revision_key(7);
    let mut folds = HashMap::new();
    folds.insert(0, TranscriptFold::Collapsed);
    assert!(
        !cache.keyed_skip_available(40, options, &folds, 2, None),
        "cell folds"
    );
    cache.note_revision_key(7);
    assert!(
        !cache.keyed_skip_available(40, options, &HashMap::new(), 3, None),
        "row count"
    );
    cache.note_revision_key(7);
    assert!(
        !cache.keyed_skip_available(
            40,
            options,
            &HashMap::new(),
            2,
            Some(TranscriptActionOwner {
                cell_index: 1,
                identity_epoch: 0
            })
        ),
        "action owner"
    );
    cache.note_revision_key(8);
    assert!(
        !cache.keyed_skip_available(40, options, &HashMap::new(), 2, None),
        "staged key must equal the accepted key"
    );
}

#[test]
fn cache_highlights_only_the_newest_user_turn() {
    let cells = vec![
        user_cell("first prompt"),
        assistant_cell("first answer", false),
        user_cell("second prompt"),
    ];
    let revisions = vec![1u64, 1, 1];

    let mut cache = TranscriptViewCache::new();
    cache.ensure(&cells, &revisions, 40, TranscriptRenderOptions::default());

    let texts = plain_lines(&cache);
    let first = texts
        .iter()
        .position(|line| line.contains("first prompt"))
        .expect("first prompt renders");
    let second = texts
        .iter()
        .position(|line| line.contains("second prompt"))
        .expect("second prompt renders");
    let lines = cache.lines();
    assert_eq!(
        lines[first].style.bg, None,
        "an older prompt renders on the bare ground"
    );
    assert!(
        lines[first]
            .spans
            .iter()
            .all(|span| span.style.bg.is_none()),
        "an older prompt paints no background block"
    );
    assert_eq!(
        lines[second].style.bg,
        Some(palette::SURFACE_ELEVATED),
        "only the newest prompt carries the background"
    );
    assert_eq!(lines[second].width(), 40);
}

#[test]
fn cache_unhighlights_the_previous_prompt_when_a_new_one_lands() {
    let first = vec![user_cell("first prompt")];
    let revisions = vec![1u64];

    let mut cache = TranscriptViewCache::new();
    cache.ensure(&first, &revisions, 40, TranscriptRenderOptions::default());
    assert_eq!(
        cache.lines()[0].style.bg,
        Some(palette::SURFACE_ELEVATED),
        "a lone prompt is the newest turn"
    );

    // The first cell's own revision never moves; supersession alone must
    // re-render it without the block.
    let both = vec![user_cell("first prompt"), user_cell("second prompt")];
    let revisions = vec![1u64, 1];
    cache.ensure(&both, &revisions, 40, TranscriptRenderOptions::default());
    let lines = cache.lines();
    assert_eq!(
        lines[0].style.bg, None,
        "the previous newest loses the background"
    );
    let texts = plain_lines(&cache);
    let second = texts
        .iter()
        .position(|line| line.contains("second prompt"))
        .expect("second prompt renders");
    assert_eq!(lines[second].style.bg, Some(palette::SURFACE_ELEVATED));
}

#[test]
fn cache_reuses_cells_when_revision_unchanged() {
    let cells = vec![
        user_cell("hello"),
        assistant_cell("world", false),
        user_cell("again"),
    ];
    let revisions = vec![1u64, 1, 1];

    let mut cache = TranscriptViewCache::new();
    cache.ensure(&cells, &revisions, 80, TranscriptRenderOptions::default());
    let first_lines: Vec<String> = cache
        .lines()
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    let first_total = cache.total_lines();
    assert!(first_total > 0, "expected non-empty render");

    // Capture per-cell lines snapshot to verify reuse.
    let snapshot_per_cell: Vec<Vec<String>> = cache
        .per_cell
        .iter()
        .map(|c| {
            c.lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        })
        .collect();

    // Same revisions => everything reused, output identical.
    cache.ensure(&cells, &revisions, 80, TranscriptRenderOptions::default());
    let second_lines: Vec<String> = cache
        .lines()
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect();
    assert_eq!(first_lines, second_lines);
    assert_eq!(cache.total_lines(), first_total);

    let snapshot_per_cell_2: Vec<Vec<String>> = cache
        .per_cell
        .iter()
        .map(|c| {
            c.lines
                .iter()
                .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
                .collect()
        })
        .collect();
    assert_eq!(snapshot_per_cell, snapshot_per_cell_2);
}

#[test]
fn bumping_one_cell_revision_only_rerenders_that_cell() {
    // Track render counts per cell using a custom HistoryCell wrapper
    // would require trait changes; instead, we detect reuse by inspecting
    // CachedCell instances. After a bump, only the bumped cell's stored
    // revision should differ from before; others remain identical.

    let cells_v1 = vec![
        user_cell("hello"),
        assistant_cell("hi", true),
        user_cell("again"),
    ];
    let revs_v1 = vec![1u64, 1, 1];

    let mut cache = TranscriptViewCache::new();
    cache.ensure(&cells_v1, &revs_v1, 80, TranscriptRenderOptions::default());

    // Snapshot the cached lines for cells 0 and 2 (unchanged across the
    // delta).
    let cell0_lines_before = cache.per_cell[0]
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let cell2_lines_before = cache.per_cell[2]
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>();

    // Mutate cell 1 (assistant streaming delta) and bump only its rev.
    let cells_v2 = vec![
        user_cell("hello"),
        assistant_cell("hi world", true),
        user_cell("again"),
    ];
    let revs_v2 = vec![1u64, 2, 1];

    cache.ensure(&cells_v2, &revs_v2, 80, TranscriptRenderOptions::default());

    // Cells 0 and 2 are byte-identical (proving reuse path didn't corrupt).
    let cell0_lines_after = cache.per_cell[0]
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    let cell2_lines_after = cache.per_cell[2]
        .lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.to_string())
                .collect::<String>()
        })
        .collect::<Vec<_>>();
    assert_eq!(cell0_lines_before, cell0_lines_after);
    assert_eq!(cell2_lines_before, cell2_lines_after);

    // Cell 1 reflects the new content.
    // The renderer interleaves role/whitespace spans, so the joined
    // content has internal padding (e.g. "Assistant   hi   world").
    // Check for the new tokens individually rather than a literal
    // "hi world" substring.
    let cell1_after: String = cache.per_cell[1]
        .lines
        .iter()
        .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        cell1_after.contains("hi") && cell1_after.contains("world"),
        "cell1 should re-render with new content; got: {cell1_after}"
    );

    // Revisions in cache reflect the bump.
    assert_eq!(cache.per_cell[0].revision, 1);
    assert_eq!(cache.per_cell[1].revision, 2);
    assert_eq!(cache.per_cell[2].revision, 1);
}

#[test]
fn streaming_assistant_keeps_a_persistent_linear_render_prefix() {
    let mut content = String::new();
    let mut revision = 1u64;
    let mut cache = TranscriptViewCache::new();
    let options = TranscriptRenderOptions {
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };

    content.push_str("start\n```rust\nlet value_0 = 0;\n```\n\n");
    let mut cells = vec![assistant_cell(&content, true)];
    cache.ensure(&cells, &[revision], 96, options);
    let lines_arc = Arc::as_ptr(&cache.per_cell[0].lines);

    for index in 1..120usize {
        let previous = revision;
        revision += 1;
        content.push_str(&format!(
            "段落 {index} e\u{301} 🚀\n```rust\nlet value_{index} = {index};\n```\n\n"
        ));
        cells[0] = assistant_cell(&content, true);
        cache.set_streaming_source_receipt(Some(StreamingSourceReceipt {
            cell_index: 0,
            from_revision: previous,
            to_revision: revision,
            content_len: content.len(),
        }));
        cache.ensure(&cells, &[revision], 96, options);
    }

    let previous = revision;
    revision += 1;
    cache.set_streaming_source_receipt(Some(StreamingSourceReceipt {
        cell_index: 0,
        from_revision: previous,
        to_revision: revision,
        content_len: content.len(),
    }));
    cache.ensure(&cells, &[revision], 96, options);

    assert_eq!(Arc::as_ptr(&cache.per_cell[0].lines), lines_arc);
    let work = cache.per_cell[0]
        .incremental_markdown
        .as_ref()
        .expect("streaming markdown cache")
        .work();
    assert_eq!(work.invalidations, 1);
    assert_eq!(work.tail_blocks_rendered, 0);
    assert_eq!(work.classified_lines as usize, content.lines().count());
    assert!(
        cache.streaming_lines_reflattened() <= (cache.total_lines() + 121) as u64,
        "flatten work must be final output plus at most one hot-tail line per update: work={}, final={}",
        cache.streaming_lines_reflattened(),
        cache.total_lines()
    );
    assert!(
        cache.streaming_meta_rows_scanned() <= 121,
        "reverse lookup must inspect only the replaceable tail: {}",
        cache.streaming_meta_rows_scanned()
    );

    let mut cold = TranscriptViewCache::new();
    cold.ensure(&cells, &[revision], 96, options);
    assert_eq!(plain_lines(&cache), plain_lines(&cold));
}

#[test]
fn tail_update_suffix_rebuild_matches_fresh_flatten() {
    let mut cells = vec![
        user_cell("first message"),
        assistant_cell("stable answer", false),
        user_cell("tail prompt"),
    ];
    let mut revisions = vec![1u64, 1, 1];
    let mut cache = TranscriptViewCache::new();
    cache.ensure(&cells, &revisions, 40, TranscriptRenderOptions::default());

    cells.push(assistant_cell("streaming tail", true));
    revisions.push(1);
    cache.ensure(&cells, &revisions, 40, TranscriptRenderOptions::default());

    if let HistoryCell::Assistant { content, .. } = cells.last_mut().unwrap() {
        content.push_str(" plus delta");
    }
    *revisions.last_mut().unwrap() += 1;
    cache.ensure(&cells, &revisions, 40, TranscriptRenderOptions::default());
    let incremental = plain_lines(&cache);

    let mut fresh = TranscriptViewCache::new();
    fresh.ensure(&cells, &revisions, 40, TranscriptRenderOptions::default());
    assert_eq!(incremental, plain_lines(&fresh));
}

#[test]
fn width_change_rerenders_all_cells() {
    let cells = vec![
        user_cell("a fairly long message that may wrap at narrow widths"),
        assistant_cell("another long message body content", false),
    ];
    let revisions = vec![5u64, 7];

    let mut cache = TranscriptViewCache::new();
    cache.ensure(&cells, &revisions, 80, TranscriptRenderOptions::default());
    let wide_total = cache.total_lines();

    // Narrow width should change layout — everything re-renders.
    cache.ensure(&cells, &revisions, 20, TranscriptRenderOptions::default());
    let narrow_total = cache.total_lines();

    assert_ne!(
        wide_total, narrow_total,
        "narrow width should produce a different number of lines"
    );

    // Restoring the original width re-renders again.
    cache.ensure(&cells, &revisions, 80, TranscriptRenderOptions::default());
    assert_eq!(cache.total_lines(), wide_total);
}

#[test]
fn streaming_assistant_only_rebuilds_one_cell_render_count() {
    // Verify behavior 6: when one Assistant cell streams a delta, only
    // that one cell is re-rendered. We use a counting wrapper hooked into
    // a custom History setup. Since `lines_with_options` is on `HistoryCell`
    // (concrete enum), we can't mock it directly. Instead we verify the
    // cache's invariant: cells with unchanged revisions retain their
    // previous CachedCell entries (clone-equal), proving no re-render
    // happened for them.
    //
    // We do this by storing revisions as monotonic u64 and verifying that
    // a `Vec<u64>` snapshot of `per_cell.revision` only differs at the
    // index that was bumped.

    let mut cells: Vec<HistoryCell> = (0..50).map(|i| user_cell(&format!("cell {i}"))).collect();
    cells.push(assistant_cell("streaming", true));
    let mut revisions: Vec<u64> = vec![1; 51];

    let mut cache = TranscriptViewCache::new();
    cache.ensure(&cells, &revisions, 80, TranscriptRenderOptions::default());

    // Snapshot total bytes rendered for cells 0..50 (unchanged).
    let stable_snapshot: Vec<String> = cache.per_cell[..50]
        .iter()
        .map(|c| {
            c.lines
                .iter()
                .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                .collect::<Vec<_>>()
                .join("|")
        })
        .collect();

    // Stream 10 deltas to the assistant cell, bumping only its revision.
    for i in 0..10 {
        if let HistoryCell::Assistant { content, .. } = &mut cells[50] {
            content.push_str(&format!(" delta-{i}"));
        }
        revisions[50] += 1;
        cache.ensure(&cells, &revisions, 80, TranscriptRenderOptions::default());

        // After every delta, cells 0..50 must be byte-identical to the
        // initial render. If we re-rendered them we'd observe identical
        // bytes anyway (deterministic), but the test ALSO checks the
        // CachedCell.revision values stayed at 1 — meaning the cache
        // never replaced them, only reused them.
        let stable_now: Vec<String> = cache.per_cell[..50]
            .iter()
            .map(|c| {
                c.lines
                    .iter()
                    .flat_map(|l| l.spans.iter().map(|s| s.content.to_string()))
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect();
        assert_eq!(
            stable_now, stable_snapshot,
            "stable cells diverged at delta {i}"
        );

        for (idx, c) in cache.per_cell[..50].iter().enumerate() {
            assert_eq!(
                c.revision, 1,
                "cell {idx} revision changed during streaming delta"
            );
        }
    }
}

#[test]
fn missing_revisions_falls_back_to_full_render() {
    // If callers pass a `cell_revisions` slice with the wrong length
    // (shouldn't happen, but be defensive), the cache should still
    // produce correct output rather than panic or skip cells.
    let cells = vec![user_cell("a"), assistant_cell("b", false)];
    let bogus_revisions = vec![1u64]; // wrong length

    let mut cache = TranscriptViewCache::new();
    cache.ensure(
        &cells,
        &bogus_revisions,
        80,
        TranscriptRenderOptions::default(),
    );

    // Both cells were rendered (no panic, output non-empty).
    assert_eq!(cache.per_cell.len(), 2);
    assert!(!cache.lines().is_empty());
}

#[test]
fn adjacent_tool_cells_render_as_one_railed_group() {
    // Live foreground exec cells collapse to a single header line (copy
    // dedupe #17), so a third cell is needed for a rail-continuation row.
    let cells = vec![
        exec_tool_cell("cargo test"),
        exec_tool_cell("cargo clippy"),
        exec_tool_cell("cargo fmt"),
    ];
    let revisions = vec![1u64, 1, 1];
    let mut cache = TranscriptViewCache::new();

    cache.ensure(&cells, &revisions, 80, TranscriptRenderOptions::default());
    let lines = plain_lines(&cache);

    assert!(
        lines
            .first()
            .is_some_and(|line| line.starts_with("\u{256D} ")),
        "first tool line should open the shared rail: {lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.starts_with("\u{2502} ")),
        "middle tool lines should continue the shared rail: {lines:?}"
    );
    assert!(
        lines
            .last()
            .is_some_and(|line| line.starts_with("\u{2570} ")),
        "last tool line should close the shared rail: {lines:?}"
    );
    assert!(
        !lines.iter().any(String::is_empty),
        "adjacent tool cells must never be separated by a bare blank row — that \
         would tear the card box open: {lines:?}"
    );
    assert!(
        !lines.iter().any(|line| line.trim_end() == "\u{2502}"),
        "one tool group must stay compact instead of padding every call: {lines:?}"
    );
    assert_eq!(spacer_rows_after_cell(&cache, 0), 0);
    assert_eq!(spacer_rows_after_cell(&cache, 1), 0);
}

#[test]
fn semantic_boundary_matrix_has_four_deliberate_rhythm_levels() {
    use TranscriptBlockKind::{Answer, DurableWork, Notice, Reasoning, ToolAction, User};
    use TranscriptBoundary::{Activity, GroupedTool, Joined, Turn};

    let cases = [
        (User, Answer, false, Turn),
        (User, ToolAction, false, Turn),
        (DurableWork, User, false, Turn),
        // Reasoning handing off to the answer is a phase change the reader
        // has to see. Running the two together with no blank row is the
        // density complaint this matrix exists to answer.
        (Reasoning, Answer, false, Activity),
        (Answer, Reasoning, false, Activity),
        // Successive cells of the *same* phase are one block split across
        // cells; a blank row there would jitter mid-stream.
        (Answer, Answer, false, Joined),
        (Reasoning, Reasoning, false, Joined),
        (Answer, ToolAction, false, Activity),
        (ToolAction, Reasoning, false, Activity),
        (Notice, DurableWork, false, Activity),
        (ToolAction, ToolAction, true, GroupedTool),
        (DurableWork, DurableWork, true, GroupedTool),
        (ToolAction, DurableWork, false, Activity),
    ];

    for (current, next, grouped_tools, expected) in cases {
        assert_eq!(
            transcript_boundary(current, next, grouped_tools),
            expected,
            "{current:?} -> {next:?}"
        );
    }

    assert_eq!(
        spacer_rows_for_boundary(Turn, TranscriptSpacing::Compact),
        1
    );
    assert_eq!(
        spacer_rows_for_boundary(Turn, TranscriptSpacing::Spacious),
        3
    );
    assert_eq!(
        spacer_rows_for_boundary(Activity, TranscriptSpacing::Compact),
        0
    );
    assert_eq!(
        spacer_rows_for_boundary(Activity, TranscriptSpacing::Spacious),
        2
    );
    assert_eq!(
        spacer_rows_for_boundary(GroupedTool, TranscriptSpacing::Compact),
        0,
        "compact density buys its density by spending no separator rows"
    );
    assert_eq!(
        spacer_rows_for_boundary(GroupedTool, TranscriptSpacing::Spacious),
        0,
        "even spacious mode breathes around the group, not inside it"
    );
}

/// Compact never spends a double blank, because a scrolling terminal cannot
/// afford it. Spacious buys its air at turn and activity seams by design.
#[test]
fn compact_never_spends_more_than_one_row_at_any_boundary() {
    use TranscriptBoundary::{Activity, GroupedTool, Joined, Turn};

    for boundary in [Joined, GroupedTool, Activity, Turn] {
        let rows = spacer_rows_for_boundary(boundary, TranscriptSpacing::Compact);
        assert!(
            rows <= BLOCK_SEPARATOR_ROWS,
            "{boundary:?} at compact spent {rows} rows"
        );
    }
}

#[test]
fn durable_work_tools_have_an_explicit_semantic_role() {
    let plan = durable_work_cell();
    let tool = exec_tool_cell("cargo test --locked");

    assert_eq!(
        TranscriptBlockKind::for_cell(&plan),
        TranscriptBlockKind::DurableWork
    );
    assert_eq!(
        TranscriptBlockKind::for_cell(&tool),
        TranscriptBlockKind::ToolAction
    );
}

#[test]
fn durable_work_starts_a_new_activity_rail_without_wasting_compact_rows() {
    let durable = HistoryCell::Tool(ToolCell::PlanUpdate(PlanUpdateCell {
        snapshot: PlanSnapshot {
            objective: Some("Keep the release receipt durable".to_string()),
            ..PlanSnapshot::default()
        },
        status: ToolStatus::Running,
    }));
    let cells = vec![
        exec_tool_cell("cargo test --locked"),
        exec_tool_cell("cargo clippy --locked"),
        durable,
    ];
    let revisions = vec![1u64; cells.len()];

    let mut compact = TranscriptViewCache::new();
    compact.ensure(
        &cells,
        &revisions,
        80,
        TranscriptRenderOptions {
            spacing: TranscriptSpacing::Compact,
            low_motion: true,
            ..TranscriptRenderOptions::default()
        },
    );

    assert_eq!(spacer_rows_after_cell(&compact, 0), 0);
    assert_eq!(spacer_rows_after_cell(&compact, 1), 0);
    let compact_lines = plain_lines(&compact);
    assert!(
        !compact_lines.iter().any(String::is_empty),
        "compact activity seams must not spend a blank row: {compact_lines:?}"
    );
    let lines_for_cell = |target| {
        compact
            .lines()
            .iter()
            .zip(compact.line_meta())
            .filter_map(|(line, meta)| match meta {
                TranscriptLineMeta::CellLine { cell_index, .. } if *cell_index == target => Some(
                    line.spans
                        .iter()
                        .map(|span| span.content.as_ref())
                        .collect::<String>(),
                ),
                TranscriptLineMeta::Spacer { .. } | TranscriptLineMeta::CellLine { .. } => None,
            })
            .collect::<Vec<_>>()
    };
    let second_action = lines_for_cell(1);
    let durable_work = lines_for_cell(2);
    assert!(
        second_action
            .last()
            .is_some_and(|line| line.starts_with("\u{2570} ")),
        "ordinary action rail should close before durable Work: {second_action:?}"
    );
    assert!(
        durable_work
            .first()
            .is_some_and(|line| line.starts_with("\u{256D} ")),
        "durable Work should open its own rail: {durable_work:?}"
    );

    let mut spacious = TranscriptViewCache::new();
    spacious.ensure(
        &cells,
        &revisions,
        80,
        TranscriptRenderOptions {
            spacing: TranscriptSpacing::Spacious,
            low_motion: true,
            ..TranscriptRenderOptions::default()
        },
    );
    assert_eq!(
        spacer_rows_after_cell(&spacious, 0),
        0,
        "two commands inside one activity rail must remain compact"
    );
    assert_eq!(
        spacer_rows_after_cell(&spacious, 1),
        2,
        "durable Work needs a semantic activity break outside compact density"
    );
}

#[test]
fn compact_spacing_keeps_conversation_blocks_separate() {
    let cells = vec![
        user_cell("Please verify the release."),
        assistant_cell("I will check the receipts.", false),
    ];
    let revisions = vec![1u64, 1];
    let mut cache = TranscriptViewCache::new();
    let options = TranscriptRenderOptions {
        spacing: TranscriptSpacing::Compact,
        ..TranscriptRenderOptions::default()
    };

    cache.ensure(&cells, &revisions, 89, options);
    let lines = plain_lines(&cache);

    assert!(
        lines.iter().any(String::is_empty),
        "compact density still needs one user/assistant boundary: {lines:?}"
    );
}

#[test]
fn compact_spacing_keeps_direct_user_tool_turns_separate() {
    let cells = vec![
        user_cell("Inspect the repository."),
        exec_tool_cell("git status --short"),
        user_cell("Now summarize the result."),
    ];
    let revisions = vec![1u64, 1, 1];
    let options = TranscriptRenderOptions {
        spacing: TranscriptSpacing::Compact,
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };
    let mut cache = TranscriptViewCache::new();

    cache.ensure(&cells, &revisions, 80, options);

    assert_eq!(spacer_rows_after_cell(&cache, 0), 1);
    assert_eq!(spacer_rows_after_cell(&cache, 1), 1);
}

#[test]
fn compact_spacing_keeps_reasoning_and_answer_in_one_response_block() {
    let cells = vec![
        HistoryCell::Thinking {
            content: "I should verify the release receipts first.".to_string(),
            streaming: false,
            duration_secs: Some(0.4),
        },
        assistant_cell("The release receipts are green.", false),
    ];
    let revisions = vec![1u64, 1];
    let mut cache = TranscriptViewCache::new();
    let options = TranscriptRenderOptions {
        spacing: TranscriptSpacing::Compact,
        ..TranscriptRenderOptions::default()
    };

    cache.ensure(&cells, &revisions, 89, options);
    let lines = plain_lines(&cache);

    assert!(
        !lines.iter().any(String::is_empty),
        "reasoning and its answer should read as one response block: {lines:?}"
    );
}

#[test]
fn hidden_reasoning_keeps_visible_rhythm_without_phantom_tail_rows() {
    let cells = vec![
        user_cell("Verify the release."),
        HistoryCell::Thinking {
            content: "Check the exact receipts.".to_string(),
            streaming: false,
            duration_secs: Some(0.4),
        },
        assistant_cell("The receipts are green.", false),
    ];
    let revisions = vec![1u64, 1, 1];
    let hidden = TranscriptRenderOptions {
        show_thinking: false,
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };
    let mut cache = TranscriptViewCache::new();

    cache.ensure(&cells, &revisions, 80, hidden);
    let hidden_lines = plain_lines(&cache);
    assert_eq!(spacer_rows_after_cell(&cache, 0), 3);
    assert!(
        hidden_lines.last().is_some_and(|line| !line.is_empty()),
        "hidden cells must not leave a trailing blank row: {hidden_lines:?}"
    );

    let visible = TranscriptRenderOptions {
        show_thinking: true,
        ..hidden
    };
    cache.ensure(&cells, &revisions, 80, visible);
    cache.ensure(&cells, &revisions, 80, hidden);
    assert_eq!(plain_lines(&cache), hidden_lines);

    let trailing_hidden = &cells[..2];
    let mut tail_cache = TranscriptViewCache::new();
    tail_cache.ensure(trailing_hidden, &revisions[..2], 80, hidden);
    assert!(
        plain_lines(&tail_cache)
            .last()
            .is_some_and(|line| !line.is_empty()),
        "a hidden final cell must not reserve a phantom spacer"
    );
}

#[test]
fn hidden_reasoning_cache_never_advertises_or_leaks_content() {
    for streaming in [false, true] {
        let cells = [reasoning_cell(streaming)];
        let mut cache = TranscriptViewCache::new();
        cache.ensure_split(
            &[&cells],
            &[1],
            80,
            TranscriptRenderOptions {
                show_thinking: false,
                ..TranscriptRenderOptions::default()
            },
            &HashMap::new(),
            None,
            Some(reasoning_owner(0)),
        );
        let text = plain_lines(&cache).join("\n");
        assert_eq!(cache.fold_action_target(), None);
        assert!(
            !text.contains("reasoning line"),
            "hidden body leaked: {text}"
        );
        assert!(!text.contains("Space:"), "hidden hint leaked: {text}");
        assert_eq!(text.contains("reasoning hidden"), streaming);
    }
}

#[test]
fn transcript_rhythm_is_width_and_reduced_motion_invariant() {
    let cells = vec![
        user_cell("Please inspect the release candidate and verify all receipts."),
        HistoryCell::Thinking {
            content: "I will inspect the source, run the checks, and compare the receipts."
                .to_string(),
            streaming: true,
            duration_secs: Some(0.8),
        },
        assistant_cell("I will start with the locked test suite.", false),
        exec_tool_cell("cargo test -p codewhale-tui --bins --locked"),
        durable_work_cell(),
        assistant_cell("The focused checks passed.", false),
        user_cell("Proceed to the final verification."),
    ];
    let revisions = vec![1u64; cells.len()];
    // user | reasoning | answer | tool | work | answer | user.
    // Turn seams breathe widest; activity seams are the interior of one turn.
    let expected_compact = [1, 0, 0, 0, 0, 1, 0];
    let expected_spacious = [3, 2, 2, 2, 2, 3, 0];

    for width in [40, 80, 100, 140] {
        for low_motion in [false, true] {
            for (spacing, expected) in [
                (TranscriptSpacing::Compact, expected_compact),
                (TranscriptSpacing::Spacious, expected_spacious),
            ] {
                let options = TranscriptRenderOptions {
                    low_motion,
                    spacing,
                    ..TranscriptRenderOptions::default()
                };
                let mut cache = TranscriptViewCache::new();
                cache.ensure(&cells, &revisions, width, options);

                let actual =
                    std::array::from_fn::<_, 7, _>(|index| spacer_rows_after_cell(&cache, index));
                assert_eq!(
                    actual, expected,
                    "width={width} low_motion={low_motion} spacing={spacing:?}"
                );
                assert!(
                    cache
                        .lines()
                        .iter()
                        .all(|line| line.width() <= usize::from(width)),
                    "render exceeded width={width} low_motion={low_motion}"
                );
            }
        }
    }
}

#[test]
fn streaming_state_transitions_do_not_move_neighbor_boundaries() {
    let mut cells = vec![
        user_cell("Inspect the candidate."),
        HistoryCell::Thinking {
            content: "Inspecting the candidate now.".to_string(),
            streaming: true,
            duration_secs: None,
        },
        exec_tool_cell("git status --short"),
        user_cell("Summarize the receipt."),
    ];
    let mut revisions = vec![1u64; cells.len()];
    let options = TranscriptRenderOptions {
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };
    let mut cache = TranscriptViewCache::new();

    let boundary_rows = |cache: &TranscriptViewCache| {
        [
            spacer_rows_after_cell(cache, 0),
            spacer_rows_after_cell(cache, 1),
            spacer_rows_after_cell(cache, 2),
        ]
    };

    cache.ensure(&cells, &revisions, 80, options);
    let first = boundary_rows(&cache);
    assert_eq!(first, [3, 2, 3]);

    cells[1] = assistant_cell("I inspected the candidate.", true);
    revisions[1] += 1;
    cache.ensure(&cells, &revisions, 80, options);
    assert_eq!(boundary_rows(&cache), first);

    cells[1] = assistant_cell("I inspected the candidate.", false);
    revisions[1] += 1;
    cache.ensure(&cells, &revisions, 80, options);
    assert_eq!(boundary_rows(&cache), first);

    let HistoryCell::Tool(ToolCell::Exec(exec)) = &mut cells[2] else {
        unreachable!("fixture is an exec tool")
    };
    exec.status = ToolStatus::Success;
    revisions[2] += 1;
    cache.ensure(&cells, &revisions, 80, options);
    assert_eq!(boundary_rows(&cache), first);
}

#[test]
fn resize_round_trip_rebuilds_the_same_semantic_rows() {
    let cells = vec![
        user_cell("A long prompt that wraps when the terminal narrows considerably."),
        exec_tool_cell("printf 'a tool receipt with a deliberately long summary'"),
        assistant_cell("A stable answer after the tool receipt.", false),
    ];
    let revisions = vec![1u64; cells.len()];
    let options = TranscriptRenderOptions {
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };
    let mut cache = TranscriptViewCache::new();

    cache.ensure(&cells, &revisions, 140, options);
    let wide = plain_lines(&cache);
    cache.ensure(&cells, &revisions, 40, options);
    cache.ensure(&cells, &revisions, 140, options);

    assert_eq!(plain_lines(&cache), wide);
    assert_eq!(cache.lines().len(), cache.line_meta().len());
    assert_eq!(cache.lines().len(), cache.line_links().len());
}

#[test]
fn palette_mode_change_invalidates_cached_syntax_rendering() {
    let cells = vec![assistant_cell(
        "```rust\nfn main() { let answer = 42; }\n```",
        false,
    )];
    let revisions = [1u64];
    let mut cache = TranscriptViewCache::new();
    let dark = TranscriptRenderOptions {
        palette_mode: palette::PaletteMode::Dark,
        ..TranscriptRenderOptions::default()
    };

    cache.ensure(&cells, &revisions, 80, dark);
    let dark_lines = Arc::clone(&cache.per_cell[0].lines);

    cache.ensure(
        &cells,
        &revisions,
        80,
        TranscriptRenderOptions {
            palette_mode: palette::PaletteMode::Light,
            ..dark
        },
    );

    assert!(
        !Arc::ptr_eq(&dark_lines, &cache.per_cell[0].lines),
        "palette mode is part of TranscriptRenderOptions and must bust cached cells"
    );
}

#[test]
fn tool_rails_preserve_rendered_width_budget() {
    let cells = vec![exec_tool_cell(
        "printf 'this is a command with enough text to wrap in narrow terminals'",
    )];
    let revisions = vec![1u64];
    let mut cache = TranscriptViewCache::new();

    cache.ensure(&cells, &revisions, 24, TranscriptRenderOptions::default());

    for line in plain_lines(&cache) {
        assert!(
            unicode_width::UnicodeWidthStr::width(line.as_str()) <= 24,
            "tool rail line exceeded narrow width: {line:?}"
        );
    }
}

/// Simulate a long, complex conversation (thinking + multi-line tool output +
/// tool headers with multiple decorative spans) and report the memory
/// consumed by `rail_prefix_widths`. This is informational — the assertion
/// only fails if the per-line overhead exceeds a generous bound.
// Test prints memory-overhead diagnostics — runs in `cargo test`, never
// inside the TUI alt-screen, so the module-level deny doesn't apply.
#[allow(clippy::print_stderr)]
#[test]
fn rail_prefix_widths_memory_overhead_complex_session() {
    let mut cells: Vec<HistoryCell> = Vec::new();
    // Build ~60 turns covering the typical deep-reasoning workflow:
    // user → thinking (5-15 lines) → assistant → tool → tool output →
    // thinking → assistant → ... repeat.
    for i in 0..30 {
        cells.push(user_cell(&format!("complex query {i} about system design")));
        cells.push(HistoryCell::Thinking {
            content:
                "line A\nline B\nline C\nline D\nline E\nline F\nline G\nline H\nline I\nline J"
                    .to_string(),
            streaming: false,
            duration_secs: Some(3.5),
        });
        cells.push(assistant_cell(
            &format!("response {i} with multi-line\ntext content spanning\nseveral lines"),
            false,
        ));
        cells.push(exec_tool_cell(
            "cargo test --package my_crate -- --nocapture 2>&1 | head -40",
        ));
        // Insert a second tool so adjacent tool cells merge into a railed group.
        cells.push(exec_tool_cell(&format!("git diff --stat HEAD~{i}")));
    }
    let revisions: Vec<u64> = (0..cells.len()).map(|i| i as u64 + 1).collect();

    let mut cache = TranscriptViewCache::new();
    cache.ensure(&cells, &revisions, 80, TranscriptRenderOptions::default());

    let total_lines = cache.total_lines();
    let pw_len = cache.rail_prefix_widths.len();
    let pw_cap = cache.rail_prefix_widths.capacity();
    // The Vec's inlined buffer on most platforms is small; capacity
    // should be >= len. Both must equal total_lines.
    assert_eq!(pw_len, total_lines);
    assert!(pw_cap >= pw_len);

    let memory_bytes = pw_cap * std::mem::size_of::<usize>();
    let memory_kb = memory_bytes as f64 / 1024.0;
    // Each usize is 8 bytes on 64-bit. Even with 100k lines this stays
    // under 1 MB.
    let kbytes_per_1k_lines = (memory_bytes as f64 / total_lines as f64) * 1000.0 / 1024.0;

    eprintln!("=== rail_prefix_widths memory (complex session) ===");
    eprintln!("  total_lines:       {total_lines}");
    eprintln!("  vec len:           {pw_len}");
    eprintln!("  vec capacity:      {pw_cap}");
    eprintln!("  memory (bytes):    {memory_bytes}");
    eprintln!("  memory (KB):       {memory_kb:.2}");
    eprintln!("  KB per 1k lines:   {kbytes_per_1k_lines:.2}");
    eprintln!("  lines × 8 bytes:   {} KB", total_lines * 8 / 1024);

    // Sanity: per-line overhead must be reasonable.
    assert!(
        memory_kb < 1024.0,
        "rail_prefix_widths memory unexpectedly large: {memory_kb:.1} KB"
    );
    eprintln!("  ✓ well under 1 MB even for very long sessions");
}

#[test]
fn ensure_filtered_matches_ensure_split_output() {
    let cells = vec![
        user_cell("hello"),
        assistant_cell("some **markdown** body", false),
        exec_tool_cell("cargo test"),
        user_cell("again"),
    ];
    let revisions = vec![1u64, 2, 3, 4];
    let index_map: Vec<usize> = vec![0, 1, 2, 3];
    // This test compares the two cache traversal paths, not animation.
    // Freeze live motion so a spinner tick between the two renders cannot
    // turn an equivalent layout into a timing-dependent failure.
    let options = TranscriptRenderOptions {
        low_motion: true,
        motion_mode: crate::tui::motion::MotionMode::Still,
        ..TranscriptRenderOptions::default()
    };

    let mut split_cache = TranscriptViewCache::new();
    split_cache.ensure_split(
        &[&cells],
        &revisions,
        40,
        options,
        &HashMap::new(),
        Some(&index_map),
        None,
    );

    let refs: Vec<&HistoryCell> = cells.iter().collect();
    let mut filtered_cache = TranscriptViewCache::new();
    filtered_cache.ensure_filtered(
        &refs,
        &revisions,
        40,
        options,
        &HashMap::new(),
        Some(&index_map),
        None,
    );

    assert_eq!(plain_lines(&split_cache), plain_lines(&filtered_cache));
    assert_eq!(
        split_cache.line_meta().len(),
        filtered_cache.line_meta().len()
    );
}

#[test]
fn ensure_filtered_reuses_unchanged_cells() {
    let cells = [
        user_cell("hello"),
        assistant_cell("streaming", true),
        user_cell("again"),
    ];
    let mut revisions = vec![1u64, 1, 1];
    let refs: Vec<&HistoryCell> = cells.iter().collect();

    let mut cache = TranscriptViewCache::new();
    cache.ensure_filtered(
        &refs,
        &revisions,
        80,
        TranscriptRenderOptions::default(),
        &HashMap::new(),
        None,
        None,
    );
    let first = plain_lines(&cache);

    cache.ensure_filtered(
        &refs,
        &revisions,
        80,
        TranscriptRenderOptions::default(),
        &HashMap::new(),
        None,
        None,
    );
    assert_eq!(first, plain_lines(&cache));
    for (idx, cached) in cache.per_cell.iter().enumerate() {
        assert_eq!(
            cached.revision, 1,
            "cell {idx} must be reused, not re-rendered"
        );
    }

    // Bump one revision: only that entry re-renders.
    revisions[1] = 2;
    cache.ensure_filtered(
        &refs,
        &revisions,
        80,
        TranscriptRenderOptions::default(),
        &HashMap::new(),
        None,
        None,
    );
    assert_eq!(cache.per_cell[0].revision, 1);
    assert_eq!(cache.per_cell[1].revision, 2);
    assert_eq!(cache.per_cell[2].revision, 1);
}

#[test]
fn prose_cells_fill_full_width_on_ultrawide_by_default() {
    // #5436: prose (user/assistant/thinking) spends the full content width
    // on wide terminals, consistent with tool cells and the #5322
    // wide-frame decision. The old 105-column rail is gone unless
    // `transcript.prose_measure` opts back into a bounded measure. The
    // cache key stays `(CellId, fed_width, revision)` so resize keeps its
    // single-feed cost model; the per-cell measure is applied inside the
    // render entry points.
    const RETIRED_RAIL_MEASURE: usize = 105;
    let long = "ultrawide prose paragraph that wraps its words across \
                    the whole terminal canvas, repeated to guarantee \
                    several wrapped rows at any column budget, "
        .repeat(6);
    let cells = [
        user_cell(&long),
        assistant_cell(&long, false),
        HistoryCell::Thinking {
            content: long.clone(),
            streaming: false,
            duration_secs: Some(2.0),
        },
        exec_tool_cell_with_output(
            "cargo test --all",
            "long tool output that itself wraps well past the prose measure ".repeat(6),
        ),
    ];
    let refs: Vec<&HistoryCell> = cells.iter().collect();
    let revisions = vec![1u64, 2, 3, 4];
    let options = TranscriptRenderOptions {
        low_motion: true,
        motion_mode: crate::tui::motion::MotionMode::Still,
        // Expanded thinking so the reasoning body also spends the width.
        verbose: true,
        thinking_default_expanded: true,
        ..TranscriptRenderOptions::default()
    };

    let mut cache = TranscriptViewCache::new();
    cache.ensure_filtered(&refs, &revisions, 220, options, &HashMap::new(), None, None);

    for idx in 0..3 {
        let width = max_line_width(&cache.per_cell[idx].lines);
        assert!(
            width > RETIRED_RAIL_MEASURE,
            "prose cell {idx} wrapped to {width} columns — still on the retired \
                 {RETIRED_RAIL_MEASURE}-column rail",
        );
        assert!(
            width <= 220,
            "prose cell {idx} wrapped to {width} columns, past the 220-column canvas",
        );
    }
    let tool_width = max_line_width(&cache.per_cell[3].lines);
    assert!(
        tool_width > RETIRED_RAIL_MEASURE,
        "tool cell must keep the full width, got {tool_width}",
    );
}

#[test]
fn transcript_prose_measure_caps_prose_but_not_tools() {
    // A positive `transcript.prose_measure` restores a bounded reading
    // measure for prose only; tool/status cells keep the full content width
    // (#5436). The 120-column cap is deliberately above the retired
    // 105-column rail so a pass proves the configured cap — not the old
    // default — is in effect.
    let long = "ultrawide ".repeat(400);
    let cells = [
        user_cell(&long),
        assistant_cell(&long, false),
        HistoryCell::Thinking {
            content: long.clone(),
            streaming: false,
            duration_secs: Some(2.0),
        },
        exec_tool_cell_with_output(
            "cargo test --all",
            "long tool output that itself wraps well past the prose measure ".repeat(6),
        ),
    ];
    let refs: Vec<&HistoryCell> = cells.iter().collect();
    let revisions = vec![1u64, 2, 3, 4];
    let options = TranscriptRenderOptions {
        low_motion: true,
        motion_mode: crate::tui::motion::MotionMode::Still,
        verbose: true,
        thinking_default_expanded: true,
        prose_measure: Some(120),
        ..TranscriptRenderOptions::default()
    };

    let mut cache = TranscriptViewCache::new();
    cache.ensure_filtered(&refs, &revisions, 220, options, &HashMap::new(), None, None);

    for idx in 0..3 {
        let width = max_line_width(&cache.per_cell[idx].lines);
        assert!(
            width > 105,
            "prose cell {idx} wrapped to {width} columns — still on the retired \
                 105-column rail, so the configured cap is not in effect",
        );
        assert!(
            width <= 120,
            "prose cell {idx} wrapped to {width} columns, over the 120-column measure",
        );
    }
    let tool_width = max_line_width(&cache.per_cell[3].lines);
    assert!(
        tool_width > 120,
        "tool cell must keep the full width past the prose measure, got {tool_width}",
    );
}

#[test]
fn overlay_and_streaming_entries_share_the_prose_measure() {
    // The main cache and the full-screen live-transcript overlay must agree
    // on the same effective prose width — the reason the measure rides on
    // `TranscriptRenderOptions` instead of being re-derived per cell
    // (#5436). Render one streaming assistant message through both
    // live-transcript entry points: the copy-metadata path (overlay) and
    // the incremental streaming path (active cell).
    let long = "ultrawide ".repeat(400);
    let cell = assistant_cell(&long, true);
    let options = TranscriptRenderOptions {
        low_motion: true,
        motion_mode: crate::tui::motion::MotionMode::Still,
        prose_measure: Some(120),
        ..TranscriptRenderOptions::default()
    };

    let overlay_lines = cell.lines_with_copy_metadata(220, options);
    let overlay_width = overlay_lines
        .iter()
        .map(|line| max_line_width(std::slice::from_ref(&line.line)))
        .max()
        .unwrap_or(0);

    let mut cache = crate::tui::markdown_render::IncrementalMarkdownRenderCache::default();
    let mut streaming_lines = Vec::new();
    let mut links = Vec::new();
    let mut separators = Vec::new();
    let mut prefix_widths = Vec::new();
    cell.update_incremental_streaming_render(
        220,
        options,
        false,
        &mut cache,
        &mut streaming_lines,
        &mut links,
        &mut separators,
        &mut prefix_widths,
    );
    let streaming_width = max_line_width(&streaming_lines);

    for (name, width) in [("overlay", overlay_width), ("streaming", streaming_width)] {
        assert!(
            width > 105,
            "{name} entry stayed on the retired 105-column rail ({width} columns)",
        );
        assert!(
            width <= 120,
            "{name} entry wrapped to {width} columns, over the 120-column measure",
        );
    }
}

#[test]
fn prose_width_resolves_the_transcript_prose_measure_contract() {
    // Absent = full content width (floored at 1); a positive cap clamps
    // from above only, so narrow terminals keep their content width
    // (#5436: 0/absent means full width).
    let full = TranscriptRenderOptions::default();
    assert_eq!(full.prose_measure, None);
    assert_eq!(full.prose_width(220), 220);
    assert_eq!(full.prose_width(96), 96);
    assert_eq!(full.prose_width(0), 1);

    let capped = TranscriptRenderOptions {
        prose_measure: Some(120),
        ..TranscriptRenderOptions::default()
    };
    assert_eq!(capped.prose_width(220), 120);
    assert_eq!(capped.prose_width(96), 96);
    assert_eq!(capped.prose_width(0), 1);
}

fn max_line_width(lines: &[Line<'static>]) -> usize {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| unicode_width::UnicodeWidthStr::width(span.content.as_ref()))
                .sum()
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn folded_thinking_cache_invalidation() {
    let long_content = "reasoning line\n".repeat(50);
    let cells = [HistoryCell::Thinking {
        content: long_content.clone(),
        streaming: false,
        duration_secs: Some(1.5),
    }];
    let revisions = [1u64];
    let options = TranscriptRenderOptions {
        verbose: true, // expanded by default
        ..TranscriptRenderOptions::default()
    };
    let width = 80u16;

    // First render: no folding → full content.
    let mut cache = TranscriptViewCache::new();
    cache.ensure_split(
        &[&cells],
        &revisions,
        width,
        options,
        &HashMap::new(),
        None,
        None,
    );
    let full_line_count = cache.total_lines();

    // Second render: fold the thinking cell → should invalidate and
    // produce fewer lines (collapsed summary).
    let mut folded = HashMap::new();
    folded.insert(0usize, TranscriptFold::Collapsed);
    cache.ensure_split(&[&cells], &revisions, width, options, &folded, None, None);
    let folded_line_count = cache.total_lines();

    assert!(
        folded_line_count < full_line_count,
        "folded thinking should render fewer lines: folded={folded_line_count} full={full_line_count}"
    );

    // Third render: unfold → should restore full content.
    cache.ensure_split(
        &[&cells],
        &revisions,
        width,
        options,
        &HashMap::new(),
        None,
        None,
    );
    let restored_line_count = cache.total_lines();
    assert_eq!(
        restored_line_count, full_line_count,
        "unfolded thinking should restore full line count"
    );
}

#[test]
fn folded_thinking_with_collapsed_cells_uses_original_indices() {
    // Two thinking cells: cell 0 and cell 1. Cell 0 is collapsed (hidden).
    // Fold cell 1 (original index 1). With the filtered index map,
    // the cache should still fold the correct cell.
    let cells = [
        HistoryCell::Thinking {
            content: "first thinking block\n".repeat(20),
            streaming: false,
            duration_secs: Some(1.0),
        },
        HistoryCell::Thinking {
            content: "second thinking block\n".repeat(20),
            streaming: false,
            duration_secs: Some(2.0),
        },
    ];
    let revisions = [1u64, 2u64];
    let options = TranscriptRenderOptions {
        verbose: true,
        ..TranscriptRenderOptions::default()
    };
    let width = 80u16;

    // No collapsing, no folding — baseline.
    let mut cache = TranscriptViewCache::new();
    cache.ensure_split(
        &[&cells],
        &revisions,
        width,
        options,
        &HashMap::new(),
        None,
        None,
    );
    let baseline = cache.total_lines();
    assert!(baseline > 0, "baseline render should contain visible lines");

    // Collapse cell 0, fold cell 1. The filtered list has only cell 1
    // at filtered index 0, but it maps to original index 1.
    let filtered_cells = [cells[1].clone()];
    let filtered_revs = [2u64];
    let index_map: Vec<usize> = vec![1]; // filtered 0 → original 1

    let mut folded = HashMap::new();
    folded.insert(1usize, TranscriptFold::Collapsed); // fold original index 1

    let mut cache2 = TranscriptViewCache::new();
    cache2.ensure_split(
        &[&filtered_cells],
        &filtered_revs,
        width,
        options,
        &folded,
        Some(&index_map),
        None,
    );
    let folded_filtered = cache2.total_lines();

    // Cell 1 was expanded in baseline; now it should be folded.
    // We can't compare directly to baseline because baseline had both
    // cells, but folded_filtered should be less than if cell 1 were
    // expanded in the filtered view.
    let mut cache3 = TranscriptViewCache::new();
    cache3.ensure_split(
        &[&filtered_cells],
        &filtered_revs,
        width,
        options,
        &HashMap::new(),
        Some(&index_map),
        None,
    );
    let expanded_filtered = cache3.total_lines();

    assert!(
        folded_filtered < expanded_filtered,
        "folded cell via index map should render fewer lines: folded={folded_filtered} expanded={expanded_filtered}"
    );
}

#[test]
fn reasoning_target_transfer_rewrites_same_revision_cells() {
    let cells = vec![reasoning_cell(false), reasoning_cell(false)];
    let revisions = [1, 2];
    let mut cache = TranscriptViewCache::new();
    let options = TranscriptRenderOptions::default();
    let hint_cells = |cache: &TranscriptViewCache| {
        cache
            .lines()
            .iter()
            .zip(cache.line_meta())
            .filter(|(line, _)| line.to_string().contains("Space:expand"))
            .filter_map(|(_, meta)| meta.cell_line().map(|(cell, _)| cell))
            .collect::<Vec<_>>()
    };

    cache.ensure_split(
        &[&cells],
        &revisions,
        80,
        options,
        &HashMap::new(),
        None,
        Some(reasoning_owner(0)),
    );
    let total = cache.total_lines();
    assert_eq!(hint_cells(&cache), vec![0]);
    let cached_lines = cache
        .per_cell
        .iter()
        .map(|cell| Arc::as_ptr(&cell.lines))
        .collect::<Vec<_>>();
    let (hint_line, hint_meta) = cache
        .lines()
        .iter()
        .zip(cache.line_meta())
        .find(|(line, _)| line.to_string().contains("Space:expand"))
        .expect("hint line");
    let hint_index = cache
        .lines()
        .iter()
        .position(|line| line.to_string().contains("Space:expand"))
        .expect("hint index");
    assert_eq!(
        hint_meta.copy_prefix_width(),
        hint_line
            .width()
            .saturating_sub(cache.rail_prefix_width(hint_index))
    );
    let (neutral_index, neutral_line, neutral_meta) = cache
        .lines()
        .iter()
        .zip(cache.line_meta())
        .enumerate()
        .find(|(_, (_, meta))| meta.cell_line() == Some((1, 0)))
        .map(|(index, (line, meta))| (index, line, meta))
        .expect("untargeted neutral affordance");
    assert_eq!(
        neutral_meta.copy_prefix_width(),
        neutral_line
            .width()
            .saturating_sub(cache.rail_prefix_width(neutral_index))
    );
    assert!(cache.line_links[neutral_index].is_empty());

    cache.retarget(Some(reasoning_owner(1)), None);
    assert_eq!(cache.total_lines(), total);
    assert_eq!(hint_cells(&cache), vec![1]);
    assert_eq!(
        cache
            .per_cell
            .iter()
            .map(|cell| Arc::as_ptr(&cell.lines))
            .collect::<Vec<_>>(),
        cached_lines,
        "target transfer must reuse neutral Markdown renders"
    );

    cache.retarget(None, None);
    assert_eq!(cache.total_lines(), total);
    assert!(hint_cells(&cache).is_empty());
}

/// Scrolling moves the Space owner to the newest visible cell. On a long
/// transcript that must repaint the two affordance rows, not re-flatten the
/// whole tail below the previous owner (#6652).
fn long_reasoning_transcript(pairs: usize) -> (Vec<HistoryCell>, Vec<u64>) {
    let cells = (0..pairs)
        .flat_map(|turn| {
            [
                reasoning_cell(false),
                assistant_cell(&format!("answer {turn}\n\nmore prose for {turn}"), false),
            ]
        })
        .collect::<Vec<_>>();
    let revisions = vec![1; cells.len()];
    (cells, revisions)
}

fn assert_same_flat_output(cache: &TranscriptViewCache, cold: &TranscriptViewCache) {
    assert_eq!(cache.lines(), cold.lines());
    assert_eq!(cache.line_links(), cold.line_links());
    assert_eq!(cache.line_meta(), cold.line_meta());
    assert_eq!(cache.rail_prefix_widths, cold.rail_prefix_widths);
    assert_eq!(cache.cell_line_starts, cold.cell_line_starts);
}

#[test]
fn retargeting_a_long_transcript_repaints_only_the_hint_rows() {
    let (cells, revisions) = long_reasoning_transcript(100);
    let options = TranscriptRenderOptions::default();
    let ensure = |cache: &mut TranscriptViewCache, owner: usize| {
        cache.ensure_split(
            &[&cells],
            &revisions,
            80,
            options,
            &HashMap::new(),
            None,
            Some(reasoning_owner(owner)),
        );
    };
    let mut cache = TranscriptViewCache::new();
    ensure(&mut cache, 0);
    assert!(cache.total_lines() > 600, "{}", cache.total_lines());

    // Scroll-frame path: `retarget` after layout.
    for owner in [2, 100, 198, 0] {
        let before = cache.streaming_lines_reflattened();
        cache.retarget(Some(reasoning_owner(owner)), None);
        assert!(
            cache.streaming_lines_reflattened() - before <= 2,
            "retarget to {owner} re-flattened {} rows",
            cache.streaming_lines_reflattened() - before
        );
        let mut cold = TranscriptViewCache::new();
        ensure(&mut cold, owner);
        assert_same_flat_output(&cache, &cold);
    }

    // Provisional-owner path: `ensure_split` with only the owner moved.
    for owner in [4, 150, 0] {
        let before = cache.streaming_lines_reflattened();
        ensure(&mut cache, owner);
        assert!(
            cache.streaming_lines_reflattened() - before <= 2,
            "ensure with owner {owner} re-flattened {} rows",
            cache.streaming_lines_reflattened() - before
        );
        let mut cold = TranscriptViewCache::new();
        ensure(&mut cold, owner);
        assert_same_flat_output(&cache, &cold);
    }

    // An answer cell owns no hint: moving there only clears the old row.
    let before = cache.streaming_lines_reflattened();
    cache.retarget(Some(reasoning_owner(1)), None);
    assert_eq!(cache.streaming_lines_reflattened() - before, 1);
    assert!(!plain_lines(&cache).join("\n").contains("Space:expand"));
}

/// While a reply streams its cell is dirty every frame. Scrolling in that
/// state must still repaint only the hint rows plus the streamed tail, not
/// re-flatten everything below the old owner (#6652).
#[test]
fn retargeting_while_streaming_repaints_only_hint_rows_and_tail() {
    let (mut cells, _) = long_reasoning_transcript(100);
    // A reasoning cell right above the streaming reply: moving the hint
    // there lands inside the rows this frame rebuilds anyway.
    cells.push(reasoning_cell(false));
    let mut content = String::from("streamed start\n\n");
    cells.push(assistant_cell(&content, true));
    let streaming = cells.len() - 1;
    let mut revisions = vec![1; cells.len()];
    let options = TranscriptRenderOptions {
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };
    let ensure = |cache: &mut TranscriptViewCache, cells: &[HistoryCell], revs: &[u64], owner| {
        cache.ensure_split(
            &[cells],
            revs,
            80,
            options,
            &HashMap::new(),
            None,
            Some(reasoning_owner(owner)),
        );
    };
    let mut cache = TranscriptViewCache::new();
    ensure(&mut cache, &cells, &revisions, 0);
    assert!(cache.total_lines() > 600, "{}", cache.total_lines());

    for (frame, owner) in [2, 100, 198, streaming - 1, 0, 4].into_iter().enumerate() {
        let previous = revisions[streaming];
        revisions[streaming] += 1;
        content.push_str(&format!("chunk {frame} of the streamed reply\n\n"));
        cells[streaming] = assistant_cell(&content, true);
        cache.set_streaming_source_receipt(Some(StreamingSourceReceipt {
            cell_index: streaming,
            from_revision: previous,
            to_revision: revisions[streaming],
            content_len: content.len(),
        }));
        let before = cache.streaming_lines_reflattened();
        ensure(&mut cache, &cells, &revisions, owner);
        let work = cache.streaming_lines_reflattened() - before;
        let tail_rows = (cache.per_cell[streaming - 1].lines.len()
            + cache.per_cell[streaming].lines.len()
            + 4) as u64;
        assert!(
            work <= tail_rows,
            "streaming frame with owner {owner} re-flattened {work} rows (tail {tail_rows})"
        );
        let mut cold = TranscriptViewCache::new();
        ensure(&mut cold, &cells, &revisions, owner);
        assert_same_flat_output(&cache, &cold);
    }
}

/// Collapsed transcripts render through an original-index map, and hidden
/// cells record their successor's line start. In-place hint repaint must
/// land on the right rows in both cases (#6652).
#[test]
fn retargeting_a_filtered_transcript_with_hidden_cells_repaints_in_place() {
    let original = (0..80)
        .flat_map(|turn| {
            [
                reasoning_cell(false),
                // Renders no rows: a hidden cell between reasoning cells.
                assistant_cell("", false),
                reasoning_cell(false),
                assistant_cell(&format!("answer {turn}\n\nmore prose for {turn}"), false),
                // Dropped by the filter, so rendered and original indices diverge.
                assistant_cell(&format!("collapsed {turn}"), false),
            ]
        })
        .collect::<Vec<_>>();
    let map = (0..original.len())
        .filter(|index| index % 5 != 4)
        .collect::<Vec<_>>();
    let refs = map
        .iter()
        .map(|&index| &original[index])
        .collect::<Vec<_>>();
    let revisions = vec![1; refs.len()];
    let options = TranscriptRenderOptions::default();
    let ensure = |cache: &mut TranscriptViewCache, owner: usize| {
        cache.ensure_filtered(
            &refs,
            &revisions,
            80,
            options,
            &HashMap::new(),
            Some(&map),
            Some(reasoning_owner(owner)),
        );
    };
    let mut cache = TranscriptViewCache::new();
    ensure(&mut cache, 0);
    assert!(cache.per_cell.iter().any(|cell| cell.is_empty));
    assert!(cache.total_lines() > 600, "{}", cache.total_lines());

    // Original indices: 2 and 7 sit after a hidden cell, 397 is the last
    // reasoning cell, and 1 is the hidden cell itself (no hint).
    for owner in [2, 150, 397, 7, 0, 1, 5] {
        let before = cache.streaming_lines_reflattened();
        cache.retarget(Some(reasoning_owner(owner)), Some(&map));
        assert!(
            cache.streaming_lines_reflattened() - before <= 2,
            "retarget to {owner} re-flattened {} rows",
            cache.streaming_lines_reflattened() - before
        );
        let mut cold = TranscriptViewCache::new();
        ensure(&mut cold, owner);
        assert_same_flat_output(&cache, &cold);

        let next = if owner == 0 { 5 } else { 0 };
        let before = cache.streaming_lines_reflattened();
        ensure(&mut cache, next);
        assert!(
            cache.streaming_lines_reflattened() - before <= 2,
            "ensure with owner {next} re-flattened {} rows",
            cache.streaming_lines_reflattened() - before
        );
        let mut cold = TranscriptViewCache::new();
        ensure(&mut cold, next);
        assert_same_flat_output(&cache, &cold);
    }
}

/// Synthetic scroll benchmark for #6652; run with `--ignored --nocapture`.
#[test]
#[ignore = "timing benchmark, not a correctness gate"]
#[allow(clippy::print_stderr)]
fn bench_retarget_scroll_over_long_transcript() {
    let (cells, revisions) = long_reasoning_transcript(2_000);
    let mut cache = TranscriptViewCache::new();
    cache.ensure_split(
        &[&cells],
        &revisions,
        120,
        TranscriptRenderOptions::default(),
        &HashMap::new(),
        None,
        Some(reasoning_owner(0)),
    );
    let before = cache.streaming_lines_reflattened();
    let started = std::time::Instant::now();
    let frames = 400;
    for frame in 0..frames {
        // Walk the owner between early reasoning cells as a scroll would.
        cache.retarget(Some(reasoning_owner((frame % 20) * 2)), None);
    }
    let elapsed = started.elapsed();
    eprintln!(
        "#6652 bench: {} lines, {frames} retargets, {:?} total, {:?}/frame, {} rows re-flattened",
        cache.total_lines(),
        elapsed,
        elapsed / frames as u32,
        cache.streaming_lines_reflattened() - before
    );
}

#[test]
fn filtered_reasoning_owner_keeps_original_identity() {
    let cells = [reasoning_cell(false)];
    let revisions = [2];
    let original_map = [1];
    let mut cache = TranscriptViewCache::new();
    cache.ensure_split(
        &[&cells],
        &revisions,
        80,
        TranscriptRenderOptions::default(),
        &HashMap::new(),
        Some(&original_map),
        Some(reasoning_owner(1)),
    );
    assert!(plain_lines(&cache).join("\n").contains("Space:expand"));
    assert_eq!(
        cache.fold_action_target(),
        Some(CellFoldActionTarget {
            owner: reasoning_owner(1),
            action: CellFoldAction::Expand,
        })
    );
    assert!(
        cache
            .line_meta()
            .iter()
            .any(|meta| meta.cell_line().is_some_and(|(rendered, _)| rendered == 0))
    );

    cache.ensure_split(
        &[&cells],
        &revisions,
        80,
        TranscriptRenderOptions::default(),
        &HashMap::new(),
        Some(&original_map),
        Some(reasoning_owner(0)),
    );
    assert!(cache.fold_action_target().is_none());
    assert!(!plain_lines(&cache).join("\n").contains("Space:expand"));
}

#[test]
fn streaming_tail_fast_path_cannot_skip_reasoning_retarget() {
    let cells = [reasoning_cell(false), assistant_cell("tail", true)];
    let mut cache = TranscriptViewCache::new();
    cache.ensure_split(
        &[&cells],
        &[1, 1],
        80,
        TranscriptRenderOptions::default(),
        &HashMap::new(),
        None,
        Some(reasoning_owner(0)),
    );
    assert!(plain_lines(&cache).join("\n").contains("Space:expand"));

    let updated = [reasoning_cell(false), assistant_cell("tail extended", true)];
    cache.ensure_split(
        &[&updated],
        &[1, 2],
        80,
        TranscriptRenderOptions::default(),
        &HashMap::new(),
        None,
        None,
    );
    assert!(cache.fold_action_target().is_none());
    assert!(!plain_lines(&cache).join("\n").contains("Space:expand"));
}

#[test]
fn narrow_reasoning_hint_never_changes_cache_geometry() {
    let cells = [reasoning_cell(false)];
    for width in 1..=40 {
        let mut cache = TranscriptViewCache::new();
        cache.ensure_split(
            &[&cells],
            &[1],
            width,
            TranscriptRenderOptions::default(),
            &HashMap::new(),
            None,
            None,
        );
        let neutral_lines = cache.total_lines();
        cache.ensure_split(
            &[&cells],
            &[1],
            width,
            TranscriptRenderOptions::default(),
            &HashMap::new(),
            None,
            Some(reasoning_owner(0)),
        );
        assert_eq!(cache.total_lines(), neutral_lines, "width {width}");
        // The terminal clips an overlong neutral header. Adding a hint must
        // never add a row or advertise a chord that cannot fit alongside it.
        if width == 40 {
            assert!(plain_lines(&cache).join("\n").contains("Space:expand"));
        }
        if width < 14 {
            assert!(!plain_lines(&cache).join("\n").contains("Space:"));
        }
    }
}

#[test]
fn reasoning_hint_uses_the_render_locale() {
    let cells = [reasoning_cell(false)];
    let options = TranscriptRenderOptions {
        locale: Locale::Ja,
        ..TranscriptRenderOptions::default()
    };
    let mut cache = TranscriptViewCache::new();
    cache.ensure_split(
        &[&cells],
        &[1],
        80,
        options,
        &HashMap::new(),
        None,
        Some(reasoning_owner(0)),
    );
    let text = plain_lines(&cache).join("\n");
    assert!(text.contains("Space:展開"), "{text}");
    assert!(!text.contains("Space:expand"), "{text}");
}

/// Rows-per-element measurement for the calm-UI PR body. Run with
/// `--ignored --nocapture`.
#[test]
#[ignore = "measurement, not a correctness gate"]
#[allow(clippy::print_stderr)]
fn measure_calm_rows_per_turn() {
    use crate::tui::history::{GenericToolCell, McpToolCell};
    let shipped = TranscriptRenderOptions {
        show_tool_details: false,
        calm_mode: true,
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };
    let body = (1..=40)
        .map(|i| format!("reasoning line {i:02}"))
        .collect::<Vec<_>>()
        .join("\n");
    let noisy = (0..30)
        .map(|i| format!("row {i:02} output"))
        .collect::<Vec<_>>()
        .join("\n");
    let generic = |name: &str, status: ToolStatus, output: Option<String>| {
        HistoryCell::Tool(ToolCell::Generic(GenericToolCell {
            name: name.to_string(),
            status,
            input_summary: Some("path: src/lib.rs".to_string()),
            output,
            prompts: None,
            spillover_path: None,
            output_summary: None,
            is_diff: false,
        }))
    };
    let mcp = |status: ToolStatus, content: String| {
        HistoryCell::Tool(ToolCell::Mcp(McpToolCell {
            tool: "mcp_linear_get_issue".to_string(),
            status,
            content: Some(content),
            is_image: false,
        }))
    };
    let turn = vec![
        user_cell("fix the bug"),
        HistoryCell::Thinking {
            content: body.clone(),
            streaming: false,
            duration_secs: Some(12.0),
        },
        mcp(
            ToolStatus::Success,
            "CW-123\nTitle\nState: Todo".to_string(),
        ),
        mcp(ToolStatus::Failed, noisy.clone()),
        generic("read_file", ToolStatus::Failed, Some(noisy.clone())),
        exec_tool_cell_with_output("cargo test", noisy.clone()),
        assistant_cell("done", false),
    ];
    for cell in &turn {
        let rows = cell.lines_with_options(80, shipped).len();
        eprintln!("calm-rows element: {rows}");
    }
    let revisions = vec![1u64; turn.len()];
    let mut cache = TranscriptViewCache::new();
    cache.ensure_split(
        &[&turn],
        &revisions,
        80,
        shipped,
        &HashMap::new(),
        None,
        None,
    );
    eprintln!("calm-rows settled turn total: {}", cache.total_lines());

    let live = vec![
        user_cell("fix the bug"),
        HistoryCell::Thinking {
            content: body,
            streaming: true,
            duration_secs: None,
        },
    ];
    let mut cache = TranscriptViewCache::new();
    let live_options = shipped;
    cache.ensure_split(
        &[&live],
        &[1, 1],
        80,
        live_options,
        &HashMap::new(),
        None,
        None,
    );
    eprintln!(
        "calm-rows streaming thinking cell (height-independent): {}",
        cache.per_cell[1].lines.len()
    );
}

#[test]
fn calm1_reasoning_header_hint_keeps_the_entire_live_tail_copyable() {
    let cells = [reasoning_cell(true)];
    let mut cache = TranscriptViewCache::new();
    let options = TranscriptRenderOptions {
        calm_mode: true,
        low_motion: true,
        ..Default::default()
    };
    cache.ensure_split(
        &[&cells],
        &[1],
        80,
        options,
        &HashMap::new(),
        None,
        Some(reasoning_owner(0)),
    );
    assert_eq!(cache.total_lines(), 4);
    assert!(cache.lines()[0].to_string().contains("Space:expand"));
    assert!(cache.lines()[3].to_string().contains("reasoning line 20"));
    for index in 1..4 {
        assert!(cache.line_meta()[index].copy_prefix_width() < cache.lines()[index].width());
    }
}

// ── Steady-state frames do no transcript work (#6652) ───────────────────────

fn settled_transcript(turns: usize) -> (Vec<HistoryCell>, Vec<u64>) {
    let cells = (0..turns)
        .flat_map(|turn| {
            [
                user_cell(&format!("question {turn}")),
                reasoning_cell(false),
                assistant_cell(&format!("answer {turn}\n\n- one\n- two"), false),
            ]
        })
        .collect::<Vec<_>>();
    let revisions = (1..=cells.len() as u64).collect();
    (cells, revisions)
}

#[test]
fn an_unchanged_frame_renders_and_moves_nothing() {
    let (cells, revisions) = settled_transcript(400);
    let options = TranscriptRenderOptions::default();
    let ensure = |cache: &mut TranscriptViewCache| {
        cache.ensure_split(
            &[&cells],
            &revisions,
            80,
            options,
            &HashMap::new(),
            None,
            Some(reasoning_owner(1)),
        );
    };
    let mut cache = TranscriptViewCache::new();
    ensure(&mut cache);
    let rendered = cache.cells_rendered;
    let reflattened = cache.streaming_lines_reflattened();
    let storage = cache.per_cell.as_ptr();
    let line_arcs: Vec<_> = cache
        .per_cell
        .iter()
        .map(|cell| Arc::as_ptr(&cell.lines))
        .collect();

    for _ in 0..5 {
        ensure(&mut cache);
    }

    assert_eq!(cache.cells_rendered, rendered, "a settled frame rendered");
    assert_eq!(cache.streaming_lines_reflattened(), reflattened);
    assert_eq!(cache.per_cell.as_ptr(), storage, "cached cells were moved");
    for (index, cell) in cache.per_cell.iter().enumerate() {
        assert_eq!(Arc::as_ptr(&cell.lines), line_arcs[index], "cell {index}");
    }
}

#[test]
fn a_tail_only_change_renders_only_the_tail_in_place() {
    let (mut cells, mut revisions) = settled_transcript(400);
    cells.push(assistant_cell("streaming", true));
    revisions.push(10_000);
    let options = TranscriptRenderOptions {
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };
    let ensure = |cache: &mut TranscriptViewCache, cells: &[HistoryCell], revisions: &[u64]| {
        cache.ensure_split(
            &[cells],
            revisions,
            80,
            options,
            &HashMap::new(),
            None,
            None,
        );
    };
    let mut cache = TranscriptViewCache::new();
    ensure(&mut cache, &cells, &revisions);
    let storage = cache.per_cell.as_ptr();
    let line_arcs: Vec<_> = cache
        .per_cell
        .iter()
        .map(|cell| Arc::as_ptr(&cell.lines))
        .collect();

    for delta in 0..20u64 {
        let before = cache.cells_rendered;
        if let Some(HistoryCell::Assistant { content, .. }) = cells.last_mut() {
            content.push_str(&format!(" delta {delta}"));
        }
        *revisions.last_mut().unwrap() += 1;
        ensure(&mut cache, &cells, &revisions);
        assert_eq!(cache.cells_rendered - before, 1, "delta {delta}");
    }
    assert_eq!(cache.per_cell.as_ptr(), storage, "cached cells were moved");
    let settled = line_arcs.len() - 1;
    for (index, cell) in cache.per_cell[..settled].iter().enumerate() {
        assert_eq!(Arc::as_ptr(&cell.lines), line_arcs[index], "cell {index}");
    }
    let mut cold = TranscriptViewCache::new();
    ensure(&mut cold, &cells, &revisions);
    assert_same_flat_output(&cache, &cold);

    // A change in the middle renders that cell and nothing after it.
    let before = cache.cells_rendered;
    cells[600] = assistant_cell("rewritten in place", false);
    revisions[600] = 20_000;
    ensure(&mut cache, &cells, &revisions);
    assert_eq!(cache.cells_rendered - before, 1);
    let mut cold = TranscriptViewCache::new();
    ensure(&mut cold, &cells, &revisions);
    assert_same_flat_output(&cache, &cold);
}

/// A filter can shift rows so that a tool cell's slot is taken by a streaming
/// answer at an unchanged cell count. The answer takes the tail-only update,
/// but the group rail and spacer between it and its neighbour belong to the
/// neighbour's rows and must be rebuilt (found by the cold-render property
/// test below).
#[test]
fn a_tool_slot_taken_by_a_streaming_answer_refreshes_its_neighbour() {
    let options = TranscriptRenderOptions {
        low_motion: true,
        ..TranscriptRenderOptions::default()
    };
    let mut cells = vec![
        user_cell("run it"),
        exec_tool_cell("cargo check"),
        exec_tool_cell("cargo test"),
    ];
    let mut revisions = vec![1u64, 2, 3];
    let ensure = |cache: &mut TranscriptViewCache, cells: &[HistoryCell], revisions: &[u64]| {
        cache.ensure_split(
            &[cells],
            revisions,
            60,
            options,
            &HashMap::new(),
            None,
            None,
        );
    };
    let mut cache = TranscriptViewCache::new();
    ensure(&mut cache, &cells, &revisions);

    cells[2] = assistant_cell("opening", true);
    revisions[2] = 4;
    ensure(&mut cache, &cells, &revisions);

    let mut cold = TranscriptViewCache::new();
    ensure(&mut cold, &cells, &revisions);
    assert_same_flat_output(&cache, &cold);
    assert!(
        !plain_lines(&cache)
            .iter()
            .any(|line| line.starts_with('╭') || line.starts_with('╰')),
        "{:?}",
        plain_lines(&cache)
    );
}

/// xorshift64*: a failing case prints its seed and step, so it replays.
struct Xorshift(u64);

impl Xorshift {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// What a live caller hands the cache, advanced the way `App` advances it:
/// every mutation takes a fresh revision from one monotonic counter, active
/// entries share the active revision, and a destructive edit moves the
/// identity epoch.
struct TranscriptModel {
    cells: Vec<HistoryCell>,
    revisions: Vec<u64>,
    next_revision: u64,
    active: Vec<HistoryCell>,
    active_revision: u64,
    folds: HashMap<usize, TranscriptFold>,
    hidden: std::collections::HashSet<usize>,
    width: u16,
    options: TranscriptRenderOptions,
    owner: Option<usize>,
    epoch: u64,
    serial: u64,
}

impl TranscriptModel {
    fn new() -> Self {
        Self {
            cells: Vec::new(),
            revisions: Vec::new(),
            next_revision: 1,
            active: Vec::new(),
            active_revision: 0,
            folds: HashMap::new(),
            hidden: Default::default(),
            width: 80,
            // Motion is wall-clock driven; the comparison is between two
            // renders taken at different instants.
            options: TranscriptRenderOptions {
                low_motion: true,
                ..TranscriptRenderOptions::default()
            },
            owner: None,
            epoch: 0,
            serial: 0,
        }
    }

    fn fresh_revision(&mut self) -> u64 {
        self.next_revision += 1;
        self.next_revision
    }

    fn random_cell(&mut self, rng: &mut Xorshift) -> HistoryCell {
        self.serial += 1;
        let serial = self.serial;
        let prose = format!(
            "{serial} {}",
            "words that wrap at narrow widths and keep going ".repeat(1 + rng.below(4))
        );
        match rng.below(9) {
            0 | 1 => user_cell(&prose),
            2 => assistant_cell(&format!("{prose}\n\n- item {serial}\n- item"), false),
            3 => assistant_cell(&prose, true),
            4 => reasoning_cell(false),
            5 => reasoning_cell(true),
            6 => exec_tool_cell(&format!("cargo test {serial}")),
            7 => exec_tool_cell_with_output(&format!("ls {serial}"), prose),
            _ => durable_work_cell(),
        }
    }

    fn mutate(&mut self, rng: &mut Xorshift) -> &'static str {
        match rng.below(16) {
            0..=3 => {
                let cell = self.random_cell(rng);
                let revision = self.fresh_revision();
                self.cells.push(cell);
                self.revisions.push(revision);
                "append"
            }
            4 | 5 if !self.cells.is_empty() => {
                let index = rng.below(self.cells.len());
                self.cells[index] = self.random_cell(rng);
                self.revisions[index] = self.fresh_revision();
                "replace"
            }
            6 => {
                // Stream into the newest cell the way the reply path does.
                let revision = self.fresh_revision();
                let streaming_tail = matches!(
                    self.cells.last(),
                    Some(HistoryCell::Assistant {
                        streaming: true,
                        ..
                    })
                );
                if streaming_tail {
                    if let Some(HistoryCell::Assistant { content, .. }) = self.cells.last_mut() {
                        content.push_str(" more streamed words\n");
                    }
                    *self.revisions.last_mut().unwrap() = revision;
                } else {
                    self.cells.push(assistant_cell("opening ", true));
                    self.revisions.push(revision);
                }
                "stream"
            }
            7 if !self.cells.is_empty() => {
                let index = self.cells.len() - 1;
                if let HistoryCell::Assistant { streaming, .. } = &mut self.cells[index] {
                    *streaming = false;
                    self.revisions[index] = self.fresh_revision();
                }
                "finish stream"
            }
            8 => {
                self.active = (0..rng.below(4)).map(|_| self.random_cell(rng)).collect();
                self.active_revision += 1;
                "replace active"
            }
            9 if !self.cells.is_empty() => {
                let index = rng.below(self.cells.len() + self.active.len());
                match rng.below(3) {
                    0 => self.folds.insert(index, TranscriptFold::Expanded),
                    1 => self.folds.insert(index, TranscriptFold::Collapsed),
                    _ => self.folds.remove(&index),
                };
                "fold"
            }
            10 => {
                self.width = [24, 40, 80, 120][rng.below(4)];
                "width"
            }
            11 => {
                self.options.spacing =
                    [TranscriptSpacing::Compact, TranscriptSpacing::Spacious][rng.below(2)];
                self.options.show_thinking = rng.below(4) != 0;
                "options"
            }
            12 if rng.below(4) == 0 => {
                self.cells.clear();
                self.revisions.clear();
                self.folds.clear();
                self.hidden.clear();
                self.epoch += 1;
                "clear"
            }
            13 if !self.cells.is_empty() => {
                // Edit/undo: roll back to an earlier prefix.
                let keep = rng.below(self.cells.len());
                self.cells.truncate(keep);
                self.revisions.truncate(keep);
                self.folds.retain(|index, _| *index < keep);
                self.hidden.retain(|index| *index < keep);
                self.epoch += 1;
                "truncate"
            }
            14 => {
                let total = self.cells.len() + self.active.len();
                if total > 0 {
                    let index = rng.below(total);
                    if !self.hidden.remove(&index) {
                        self.hidden.insert(index);
                    }
                }
                "hide"
            }
            15 => {
                let total = self.cells.len() + self.active.len();
                self.owner = (total > 0 && rng.below(3) != 0).then(|| rng.below(total));
                "owner"
            }
            _ => "no-op",
        }
    }

    /// One live frame: committed history plus the active tail, filtered when
    /// the user has hidden cells, exactly as the chat widget assembles it.
    fn frame(&self, cache: &mut TranscriptViewCache, owner: Option<usize>) {
        let total = self.cells.len() + self.active.len();
        let owner = owner
            .filter(|index| *index < total)
            .map(|cell_index| TranscriptActionOwner {
                cell_index,
                identity_epoch: self.epoch,
            });
        let active_revisions = (0..self.active.len()).map(|i| {
            crate::tui::widgets::active_entry_revision(self.active_revision, i as u64 + 1)
        });
        if self.hidden.is_empty() {
            let revisions: Vec<u64> = self
                .revisions
                .iter()
                .copied()
                .chain(active_revisions)
                .collect();
            cache.ensure_split(
                &[&self.cells, &self.active],
                &revisions,
                self.width,
                self.options,
                &self.folds,
                None,
                owner,
            );
            return;
        }
        let revisions: Vec<u64> = self
            .revisions
            .iter()
            .copied()
            .chain(active_revisions)
            .collect();
        let mut kept = Vec::new();
        let mut kept_revisions = Vec::new();
        let mut map = Vec::new();
        for (index, cell) in self.cells.iter().chain(&self.active).enumerate() {
            if !self.hidden.contains(&index) {
                kept.push(cell);
                kept_revisions.push(revisions[index]);
                map.push(index);
            }
        }
        cache.ensure_filtered(
            &kept,
            &kept_revisions,
            self.width,
            self.options,
            &self.folds,
            Some(&map),
            owner,
        );
    }
}

/// After every mutation, the incrementally maintained cache must equal one
/// built from scratch from the same state: the early-out and in-place update
/// may change how much work a frame does, never what it shows.
#[test]
fn cached_transcript_matches_a_cold_render_after_every_mutation() {
    for seed in 1..=8u64 {
        let mut rng = Xorshift(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut model = TranscriptModel::new();
        let mut warm = TranscriptViewCache::new();
        let mut log = Vec::new();
        for step in 0..150 {
            let op = model.mutate(&mut rng);
            log.push(op);
            let context = || format!("seed {seed} step {step}: {}", log.join(", "));

            let owner = model.owner;
            model.frame(&mut warm, owner);
            let mut cold = TranscriptViewCache::new();
            model.frame(&mut cold, owner);
            assert_eq!(warm.lines(), cold.lines(), "{}", context());
            assert_eq!(warm.line_links(), cold.line_links(), "{}", context());
            assert_eq!(warm.line_meta(), cold.line_meta(), "{}", context());
            assert_eq!(
                warm.rail_prefix_widths,
                cold.rail_prefix_widths,
                "{}",
                context()
            );
            assert_eq!(
                warm.cell_line_starts,
                cold.cell_line_starts,
                "{}",
                context()
            );
            assert_eq!(
                warm.fold_action_target(),
                cold.fold_action_target(),
                "{}",
                context()
            );

            // The same inputs again are a settled frame: no render, no rows
            // re-flattened, identical output.
            let rendered = warm.cells_rendered;
            let reflattened = warm.streaming_lines_reflattened();
            model.frame(&mut warm, owner);
            assert_eq!(warm.cells_rendered, rendered, "{}", context());
            assert_eq!(
                warm.streaming_lines_reflattened(),
                reflattened,
                "{}",
                context()
            );
            assert_eq!(warm.lines(), cold.lines(), "{}", context());

            // A scroll frame retargets the hint after layout.
            if rng.below(3) == 0 {
                let total = model.cells.len() + model.active.len();
                let retarget = (total > 0).then(|| rng.below(total));
                let map: Vec<usize> = (0..total).filter(|i| !model.hidden.contains(i)).collect();
                let index_map = (!model.hidden.is_empty()).then_some(map.as_slice());
                let epoch = model.epoch;
                warm.retarget(
                    retarget.map(|cell_index| TranscriptActionOwner {
                        cell_index,
                        identity_epoch: epoch,
                    }),
                    index_map,
                );
                let mut cold = TranscriptViewCache::new();
                model.frame(&mut cold, retarget);
                assert_eq!(warm.lines(), cold.lines(), "retarget; {}", context());
                assert_eq!(
                    warm.line_meta(),
                    cold.line_meta(),
                    "retarget; {}",
                    context()
                );
            }
        }
    }
}
