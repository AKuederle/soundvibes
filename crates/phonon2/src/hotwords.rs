//! Sparse hotword tables from fermion-research 0.2.11's Apache-2.0 hotwords.py.
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

use crate::{Error, Result};

pub(crate) struct Bias {
    pub table: Vec<i32>,
    pub tokens: Vec<i32>,
    pub columns: Vec<i32>,
    pub depth: Vec<i32>,
    pub strength: f32,
}

impl Bias {
    pub fn compile(words: &[String], vocabulary: &[String], strength: f32) -> Result<Option<Self>> {
        if words.is_empty() || strength == 0.0 {
            return Ok(None);
        }
        let mut pieces = HashMap::new();
        for (id, piece) in vocabulary.iter().enumerate() {
            if !piece.is_empty()
                && !(piece.starts_with("<|") && piece.ends_with("|>"))
                && piece != "<unk>"
                && piece != "<pad>"
            {
                pieces.entry(piece.as_str()).or_insert(id as i32);
            }
        }
        let max_piece = pieces.keys().map(|piece| piece.len()).max().unwrap_or(1);
        let mut sequences = BTreeSet::new();
        for word in words {
            let word = word.split_whitespace().collect::<Vec<_>>().join(" ");
            if word.is_empty() {
                continue;
            }
            let capitalized = word
                .split(' ')
                .map(|part| {
                    let mut chars = part.chars();
                    chars.next().map_or_else(String::new, |first| {
                        first.to_uppercase().collect::<String>() + chars.as_str()
                    })
                })
                .collect::<Vec<_>>()
                .join(" ");
            for variant in [&word, &word.to_lowercase(), &capitalized] {
                let surface = format!("▁{}", variant.replace(' ', "▁"));
                sequences.extend(segmentations(&surface, &pieces, max_piece));
            }
        }

        let mut edges = vec![BTreeMap::new()];
        let mut depth = vec![0];
        for sequence in sequences {
            let mut state = 0;
            for token in sequence {
                let next = if let Some(&next) = edges[state].get(&token) {
                    next
                } else {
                    if edges.len() == 8192 {
                        return Err(Error::new("hotword automaton exceeds 8192 states"));
                    }
                    let next = edges.len();
                    edges[state].insert(token, next);
                    edges.push(BTreeMap::new());
                    depth.push(depth[state] + 1);
                    next
                };
                state = next;
            }
        }
        if edges.len() == 1 {
            return Ok(None);
        }
        let tokens: Vec<i32> = edges
            .iter()
            .flat_map(|row| row.keys().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let mut columns = vec![-1; vocabulary.len() + 1];
        for (column, &token) in tokens.iter().enumerate() {
            columns[token as usize] = column as i32;
        }
        let width = tokens.len();
        let mut table = vec![0; edges.len() * width];
        for (&token, &next) in &edges[0] {
            table[columns[token as usize] as usize] = next as i32;
        }
        let mut failure = vec![0; edges.len()];
        let mut queue: VecDeque<usize> = edges[0].values().copied().collect();
        while let Some(state) = queue.pop_front() {
            for column in 0..width {
                table[state * width + column] = table[failure[state] * width + column];
            }
            for (&token, &next) in &edges[state] {
                let column = columns[token as usize] as usize;
                failure[next] = table[failure[state] * width + column] as usize;
                table[state * width + column] = next as i32;
                queue.push_back(next);
            }
        }
        Ok(Some(Self {
            table,
            tokens,
            columns,
            depth,
            strength,
        }))
    }
}

/// Keep the same six segmentations per suffix and traversal order as upstream.
fn segmentations(text: &str, pieces: &HashMap<&str, i32>, max_piece: usize) -> Vec<Vec<i32>> {
    let mut suffixes: Vec<Vec<Vec<i32>>> = vec![Vec::new(); text.len() + 1];
    suffixes[text.len()].push(Vec::new());
    for (position, _) in text.char_indices().rev() {
        let mut paths = Vec::new();
        'pieces: for end in (position + 1..=text.len().min(position + max_piece)).rev() {
            let Some(&token) = text.get(position..end).and_then(|s| pieces.get(s)) else {
                continue;
            };
            for tail in &suffixes[end] {
                let mut sequence = Vec::with_capacity(tail.len() + 1);
                sequence.push(token);
                sequence.extend(tail);
                paths.push(sequence);
                if paths.len() == 24 {
                    break 'pieces;
                }
            }
        }
        paths.sort_by_key(Vec::len);
        paths.truncate(6);
        suffixes[position] = paths;
    }
    std::mem::take(&mut suffixes[0])
}
