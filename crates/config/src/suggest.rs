#[must_use]
pub fn edit_distance(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    if a.is_empty() {
        return b.chars().count();
    }
    if b.is_empty() {
        return a.chars().count();
    }

    let b_chars: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b_chars.len()).collect();
    let mut current = vec![0usize; b_chars.len() + 1];

    for (i, a_ch) in a.chars().enumerate() {
        current[0] = i + 1;
        for (j, b_ch) in b_chars.iter().enumerate() {
            let cost = if a_ch == *b_ch { 0 } else { 1 };
            let delete = previous[j + 1] + 1;
            let insert = current[j] + 1;
            let substitute = previous[j] + cost;
            current[j + 1] = delete.min(insert).min(substitute);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[b_chars.len()]
}

#[must_use]
pub fn best_suggestion_score<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<(u8, usize)> {
    let mut best: Option<(u8, usize)> = None;
    for candidate in candidates {
        let prefix_match = candidate.starts_with(query) || query.starts_with(candidate);
        let contains_match = candidate.contains(query) || query.contains(candidate);
        let distance = edit_distance(candidate, query);
        let close_typo = distance <= 2;
        if !(prefix_match || contains_match || close_typo) {
            continue;
        }

        let rank = if prefix_match {
            0
        } else if contains_match {
            1
        } else {
            2
        };

        match best {
            Some((best_rank, best_distance))
                if rank > best_rank || (rank == best_rank && distance >= best_distance) => {}
            _ => best = Some((rank, distance)),
        }
    }
    best
}
