use super::{
    AppState, BTreeMap, ChatMessage, GatewayError, HashMap, HashSet, Role, ToolCall,
    ToolCallRecord, ToolResult, Uuid, build_tool_round_messages,
    provider_record::known_secret_values,
};
use swage_core::secret_masking::{REDACTED_SECRET, mask_json_strings, mask_record_text};

#[cfg(test)]
pub(super) async fn rebuild_thread_history_messages(
    storage: &dyn swage_storage::store::Storage,
    thread_id: Uuid,
) -> Result<Vec<ChatMessage>, GatewayError> {
    rebuild_thread_history(storage, thread_id, None, Some(&[])).await
}

pub(super) async fn rebuild_thread_history_messages_with_retrieval(
    state: &AppState,
    thread_id: Uuid,
    retrieval: swage_core::ReplayRetrieval,
) -> Result<Vec<ChatMessage>, GatewayError> {
    let secret_values = match known_secret_values(state).await {
        Ok(values) => Some(values),
        Err(_) => {
            tracing::warn!(%thread_id, "omitting replayed arguments and results because secret redaction is unavailable");
            None
        }
    };
    rebuild_thread_history(
        state.storage.as_ref(),
        thread_id,
        Some(retrieval),
        secret_values.as_deref(),
    )
    .await
}

async fn rebuild_thread_history(
    storage: &dyn swage_storage::store::Storage,
    thread_id: Uuid,
    retrieval: Option<swage_core::ReplayRetrieval>,
    secret_values: Option<&[String]>,
) -> Result<Vec<ChatMessage>, GatewayError> {
    let stored_history = storage.list_messages_by_thread(thread_id).await?;
    let mut rebuilt = Vec::new();
    let mut replay_inserted_for_turn = HashSet::new();

    // A turn's durable tool rounds replay before its assistant message. A turn
    // that never reached one — interrupted mid-tool — still replays its
    // rounds after its last user message, so a resumed
    // continuation sees the calls that already ran (and those cut off) instead
    // of selecting them again.
    let turns_with_assistant_message: HashSet<Uuid> = stored_history
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .filter_map(|message| message.turn_id)
        .collect();

    let mut replay_anchor = HashMap::new();
    for (index, message) in stored_history.iter().enumerate() {
        if let Some(turn_id) = message.turn_id {
            replay_anchor.insert(turn_id, index);
        }
    }
    for (index, message) in stored_history.iter().enumerate() {
        if message.role == Role::User
            && let Some(turn_id) = message.turn_id
        {
            replay_anchor.insert(turn_id, index);
        }
    }

    for (index, message) in stored_history.into_iter().enumerate() {
        let replay_turn = message.turn_id.filter(|turn_id| {
            !replay_inserted_for_turn.contains(turn_id)
                && (message.role == Role::Assistant
                    || (!turns_with_assistant_message.contains(turn_id)
                        && replay_anchor.get(turn_id) == Some(&index)))
        });
        let replay_before = replay_turn.filter(|_| message.role == Role::Assistant);
        let replay_after = replay_turn.filter(|_| message.role != Role::Assistant);

        if let Some(turn_id) = replay_before {
            replay_inserted_for_turn.insert(turn_id);
            let tool_calls = storage.list_tool_calls_by_turn(turn_id).await?;
            rebuilt.extend(compact_tool_replay_messages_with_retrieval(
                &tool_calls,
                retrieval,
                secret_values,
            )?);
        }

        match message.role {
            Role::Tool => {
                if let Some(call_id) = message.tool_call_id {
                    rebuilt.push(ChatMessage::tool_result(call_id, message.content));
                }
            }
            role => {
                let attachments = super::message_attachments(&message.attachments)?;
                rebuilt.push(ChatMessage::new_parts(
                    role,
                    super::content_parts(message.content, &attachments),
                ));
            }
        }

        if let Some(turn_id) = replay_after {
            replay_inserted_for_turn.insert(turn_id);
            let tool_calls = storage.list_tool_calls_by_turn(turn_id).await?;
            rebuilt.extend(compact_tool_replay_messages_with_retrieval(
                &tool_calls,
                retrieval,
                secret_values,
            )?);
        }
    }

    Ok(rebuilt)
}

#[cfg(test)]
pub(super) fn compact_tool_replay_messages(
    records: &[ToolCallRecord],
) -> Result<Vec<ChatMessage>, swage_core::StorageError> {
    compact_tool_replay_messages_with_retrieval(records, None, Some(&[]))
}

fn compact_tool_replay_messages_with_retrieval(
    records: &[ToolCallRecord],
    retrieval: Option<swage_core::ReplayRetrieval>,
    secret_values: Option<&[String]>,
) -> Result<Vec<ChatMessage>, swage_core::StorageError> {
    let parent_rounds: HashMap<&str, i32> = records
        .iter()
        .filter(|record| record.parent_call_id.is_none())
        .map(|record| (record.call_id.as_str(), record.provider_round))
        .collect();
    let mut records_by_round: BTreeMap<i32, Vec<&ToolCallRecord>> = BTreeMap::new();

    for record in records.iter().filter(|record| {
        (record.model_result.is_some() || (record.result.is_none() && record.status.is_none()))
            && !record.tool.trim().is_empty()
    }) {
        let effective_round = match record.parent_call_id.as_deref() {
            Some(parent_call_id) => {
                parent_rounds.get(parent_call_id).copied().ok_or_else(|| {
                    swage_core::StorageError::Corruption {
                        message: format!(
                            "tool call '{}' references missing parent call '{parent_call_id}'",
                            record.call_id
                        ),
                    }
                })?
            }
            None => record.provider_round,
        };
        records_by_round
            .entry(effective_round)
            .or_default()
            .push(record);
    }

    let mut replay_messages = Vec::new();
    for mut round_records in records_by_round.into_values() {
        round_records.sort_by_key(|record| record.position);
        let mut tool_calls = Vec::new();
        let mut tool_results = Vec::new();

        for record in round_records {
            let model_result = replay_result(record, secret_values);
            let model_result = match retrieval {
                Some(retrieval) => with_elision_note(model_result, retrieval),
                None => model_result,
            };
            tool_calls.push(ToolCall {
                call_id: record.call_id.clone(),
                name: record.tool.clone(),
                arguments: match secret_values {
                    Some(values) => replay_arguments(record, values),
                    None => serde_json::json!({}),
                },
            });
            tool_results.push(ToolResult {
                call_id: record.call_id.clone(),
                content: model_result,
                is_error: record.status.as_deref() != Some("success"),
            });
        }

        if !tool_calls.is_empty() {
            replay_messages.extend(build_tool_round_messages(&tool_calls, &tool_results, ""));
        }
    }

    Ok(replay_messages)
}

/// The replay as a later turn reads it: a shortened result carries the note
/// that says so. An account of the call's approval can precede the
/// single-line envelope; it is kept as it is, ahead of the noted envelope.
fn with_elision_note(model_result: String, retrieval: swage_core::ReplayRetrieval) -> String {
    let (account, envelope) = match model_result.rsplit_once('\n') {
        Some((account, envelope)) => (Some(account), envelope),
        None => (None, model_result.as_str()),
    };
    let Ok(replay) = serde_json::from_str::<swage_core::CompactToolReplay>(envelope) else {
        return model_result;
    };
    let envelope = replay.with_elision_note(retrieval).to_stable_json();
    match account {
        Some(account) => format!("{account}\n{envelope}"),
        None => envelope,
    }
}

fn replay_result(record: &ToolCallRecord, secret_values: Option<&[String]>) -> String {
    let Some(stored) = &record.model_result else {
        // A started row records the call but its outcome is unknown.
        return swage_core::compact_tool_result_for_model_with_reference(
            swage_storage::store::INTERRUPTED_TOOL_RESULT,
            true,
            swage_core::tool_result_reference(&record.turn_id.to_string(), &record.call_id),
        );
    };
    let Some(secret_values) = secret_values else {
        return REDACTED_SECRET.into();
    };
    if let Some(mut result) = crate::tool::tool_result_read::recovered_model_result(record) {
        mask_record_text(&mut result, secret_values);
        return result;
    }
    if secret_values.is_empty() {
        return stored.clone();
    }

    // An approval account can precede the single-line replay envelope.
    let (account, envelope) = stored.rsplit_once('\n').unwrap_or(("", stored));
    let Ok(replay) = serde_json::from_str::<swage_core::CompactToolReplay>(envelope) else {
        let mut result = stored.clone();
        mask_record_text(&mut result, secret_values);
        return result;
    };
    let mut result = if replay.truncated {
        // The preview can end inside a secret. Check the complete
        // model-facing result before applying the preview limit again.
        let Some(full_result) = verified_full_result(record, &replay) else {
            tracing::warn!(
                turn_id = %record.turn_id,
                call_id = %record.call_id,
                "omitting replayed result because the full result could not be verified"
            );
            return REDACTED_SECRET.into();
        };
        full_result
    } else {
        replay.preview
    };
    let mut account = account.to_owned();
    let account_changed = mask_record_text(&mut account, secret_values);
    let result_changed = mask_record_text(&mut result, secret_values);
    if !account_changed && !result_changed {
        return stored.clone();
    }
    let envelope = if result_changed {
        let is_error = replay
            .is_error
            .unwrap_or(record.status.as_deref() != Some("success"));
        match replay.full_result_ref {
            Some(reference) => swage_core::compact_tool_result_for_model_with_reference(
                &result, is_error, reference,
            ),
            None => swage_core::compact_tool_result_for_model(&result, is_error),
        }
    } else {
        envelope.to_owned()
    };
    if account.is_empty() {
        envelope
    } else {
        format!("{account}\n{envelope}")
    }
}

fn verified_full_result(
    record: &ToolCallRecord,
    replay: &swage_core::CompactToolReplay,
) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::sync::LazyLock;
    use swage_safety::LeakDetector;

    static DETECTOR: LazyLock<LeakDetector> = LazyLock::new(LeakDetector::default);

    let original = record.result.as_deref()?;
    let matches_replay = |text: &str| {
        text.len() == replay.bytes
            && text.starts_with(replay.preview.as_str())
            && Sha256::digest(text.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
                == replay.sha256
    };
    if matches_replay(original) {
        return Some(original.to_owned());
    }
    // The stored digest is of the model-facing result after pattern scanning.
    let scanned = DETECTOR.scan(original);
    matches_replay(&scanned).then_some(scanned)
}

/// A call's arguments as history replays them: decoded from the one form the
/// store keeps, masked against the values registered since they were
/// written, and with each long string value bounded. Masking comes first,
/// since a bound could leave the start of a value in place. Arguments that do
/// not decode replay as none, so the call's result still reaches the model.
fn replay_arguments(record: &ToolCallRecord, secret_values: &[String]) -> serde_json::Value {
    let Ok(mut arguments) = serde_json::from_str(&record.arguments) else {
        tracing::warn!(
            turn_id = %record.turn_id,
            call_id = %record.call_id,
            "replaying a call without its arguments because they could not be decoded"
        );
        return serde_json::json!({});
    };
    mask_json_strings(&mut arguments, secret_values);
    bounded_arguments(&arguments)
}

fn bounded_arguments(arguments: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(&swage_core::compact_tool_arguments_for_model(arguments))
        .expect("compacted tool arguments are valid JSON")
}

#[cfg(test)]
mod tests {
    use super::with_elision_note;
    use swage_core::{CompactToolReplay, ReplayRetrieval};

    /// An approval's account of the call can precede the stored envelope;
    /// a shortened result says it is shortened all the same, and the account
    /// is kept ahead of it as it was.
    #[test]
    fn a_shortened_result_after_an_approval_account_carries_the_note() {
        let envelope = swage_core::compact_tool_result_for_model_with_reference(
            &"x".repeat(5_000),
            false,
            "tool_call:00000000-0000-0000-0000-000000000000:call-1:result",
        );
        let account = "The person approved this call.";
        let replayed = with_elision_note(
            format!("{account}\n{envelope}"),
            ReplayRetrieval::ByReference,
        );

        let (kept, noted) = replayed.rsplit_once('\n').expect("account and envelope");
        assert_eq!(kept, account);
        let noted: CompactToolReplay = serde_json::from_str(noted).unwrap();
        assert!(noted.truncated);
        assert!(
            noted
                .note
                .as_deref()
                .is_some_and(|note| note.contains("tool_result_read")),
            "the note names the tool that reads the rest"
        );
    }
}
