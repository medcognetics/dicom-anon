//! Constrained BIOES Viterbi decoding for the fixed 33-label model head.

use crate::{Error, Result};
use serde::Deserialize;

const LABEL_COUNT: usize = 33;
const BACKGROUND_LABEL: usize = 0;
const NEGATIVE_INFINITY: f32 = -1.0e9;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Begin,
    Inside,
    End,
    Single,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub(crate) struct ViterbiBiases {
    pub(crate) transition_bias_background_stay: f32,
    pub(crate) transition_bias_background_to_start: f32,
    pub(crate) transition_bias_inside_to_continue: f32,
    pub(crate) transition_bias_inside_to_end: f32,
    pub(crate) transition_bias_end_to_background: f32,
    pub(crate) transition_bias_end_to_start: f32,
}

/// Constrained Viterbi decoder for OpenAI Privacy Filter BIOES logits.
#[derive(Debug, Clone)]
pub struct ViterbiDecoder {
    start_scores: [f32; LABEL_COUNT],
    end_scores: [f32; LABEL_COUNT],
    transition_scores: [[f32; LABEL_COUNT]; LABEL_COUNT],
}

impl ViterbiDecoder {
    /// Construct the decoder with the model's default zero transition biases.
    #[must_use]
    pub fn openai_default() -> Self {
        Self::with_biases(ViterbiBiases::default())
    }

    pub(crate) fn with_biases(biases: ViterbiBiases) -> Self {
        let mut decoder = Self {
            start_scores: [NEGATIVE_INFINITY; LABEL_COUNT],
            end_scores: [NEGATIVE_INFINITY; LABEL_COUNT],
            transition_scores: [[NEGATIVE_INFINITY; LABEL_COUNT]; LABEL_COUNT],
        };
        for label in 0..LABEL_COUNT {
            if is_background(label)
                || matches!(boundary(label), Some(Boundary::Begin | Boundary::Single))
            {
                decoder.start_scores[label] = 0.0;
            }
            if is_background(label)
                || matches!(boundary(label), Some(Boundary::End | Boundary::Single))
            {
                decoder.end_scores[label] = 0.0;
            }
            for next in 0..LABEL_COUNT {
                if valid_transition(label, next) {
                    decoder.transition_scores[label][next] = transition_bias(label, next, biases);
                }
            }
        }
        decoder
    }

    /// Decode one `[sequence, 33]` logit matrix into label identifiers.
    pub fn decode(&self, logits: &[Vec<f32>]) -> Result<Vec<usize>> {
        if logits.is_empty() {
            return Ok(Vec::new());
        }
        if logits.iter().any(|row| row.len() != LABEL_COUNT) {
            return Err(Error::Model(
                "model logits must contain exactly 33 classes per token".to_owned(),
            ));
        }
        if logits.iter().flatten().any(|score| !score.is_finite()) {
            return Err(Error::Model(
                "model logits contain a non-finite value".to_owned(),
            ));
        }

        let emissions = logits
            .iter()
            .map(|row| log_softmax(row))
            .collect::<Vec<_>>();
        let mut scores = [0.0_f32; LABEL_COUNT];
        for label in 0..LABEL_COUNT {
            scores[label] = emissions[0][label] + self.start_scores[label];
        }
        let mut backpointers = vec![[0_usize; LABEL_COUNT]; logits.len() - 1];
        for token in 1..logits.len() {
            let mut next_scores = [NEGATIVE_INFINITY; LABEL_COUNT];
            for next in 0..LABEL_COUNT {
                let mut best_previous = 0;
                let mut best_score = NEGATIVE_INFINITY;
                for (previous, score) in scores.iter().enumerate() {
                    let candidate = score + self.transition_scores[previous][next];
                    if candidate > best_score {
                        best_score = candidate;
                        best_previous = previous;
                    }
                }
                next_scores[next] = best_score + emissions[token][next];
                backpointers[token - 1][next] = best_previous;
            }
            scores = next_scores;
        }

        let mut last_label = 0;
        let mut last_score = NEGATIVE_INFINITY;
        for (label, score) in scores.iter().enumerate() {
            let candidate = score + self.end_scores[label];
            if candidate > last_score {
                last_score = candidate;
                last_label = label;
            }
        }
        let mut path = vec![0; logits.len()];
        *path.last_mut().expect("non-empty path") = last_label;
        for token in (0..logits.len() - 1).rev() {
            last_label = backpointers[token][last_label];
            path[token] = last_label;
        }
        Ok(path)
    }
}

fn transition_bias(previous: usize, next: usize, biases: ViterbiBiases) -> f32 {
    if is_background(previous) {
        if is_background(next) {
            return biases.transition_bias_background_stay;
        }
        if matches!(boundary(next), Some(Boundary::Begin | Boundary::Single)) {
            return biases.transition_bias_background_to_start;
        }
    }
    if matches!(boundary(previous), Some(Boundary::Begin | Boundary::Inside)) {
        if boundary(next) == Some(Boundary::Inside)
            && span_category(previous) == span_category(next)
        {
            return biases.transition_bias_inside_to_continue;
        }
        if boundary(next) == Some(Boundary::End) && span_category(previous) == span_category(next) {
            return biases.transition_bias_inside_to_end;
        }
    }
    if matches!(boundary(previous), Some(Boundary::End | Boundary::Single)) {
        if is_background(next) {
            return biases.transition_bias_end_to_background;
        }
        if matches!(boundary(next), Some(Boundary::Begin | Boundary::Single)) {
            return biases.transition_bias_end_to_start;
        }
    }
    0.0
}

fn log_softmax(logits: &[f32]) -> Vec<f32> {
    let maximum = logits.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let normalizer = logits
        .iter()
        .map(|value| (*value - maximum).exp())
        .sum::<f32>()
        .ln()
        + maximum;
    logits.iter().map(|value| *value - normalizer).collect()
}

const fn is_background(label: usize) -> bool {
    label == BACKGROUND_LABEL
}

const fn span_category(label: usize) -> Option<usize> {
    if is_background(label) {
        None
    } else {
        Some((label - 1) / 4)
    }
}

const fn boundary(label: usize) -> Option<Boundary> {
    if is_background(label) {
        return None;
    }
    match (label - 1) % 4 {
        0 => Some(Boundary::Begin),
        1 => Some(Boundary::Inside),
        2 => Some(Boundary::End),
        3 => Some(Boundary::Single),
        _ => None,
    }
}

fn valid_transition(previous: usize, next: usize) -> bool {
    if is_background(previous) {
        return is_background(next)
            || matches!(boundary(next), Some(Boundary::Begin | Boundary::Single));
    }
    match boundary(previous) {
        Some(Boundary::Begin | Boundary::Inside) => {
            span_category(previous) == span_category(next)
                && matches!(boundary(next), Some(Boundary::Inside | Boundary::End))
        }
        Some(Boundary::End | Boundary::Single) => {
            is_background(next)
                || matches!(boundary(next), Some(Boundary::Begin | Boundary::Single))
        }
        None => false,
    }
}
