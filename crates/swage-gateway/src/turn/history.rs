use super::{
    BTreeMap, ChatMessage, GatewayError, HashMap, HashSet, Role, ToolCall, ToolCallRecord,
    ToolResult, Uuid, build_tool_round_messages,
};

#[cfg(test)]
pub(super) async fn rebuild_thread_history_messages(
    storage: &dyn swage_storage::store::Storage,
    thread_id: Uuid,
) -> Result<Vec<ChatMessage>, GatewayError> {
    rebuild_thread_history_messages_with_retrieval(storage, thread_id, None).await
}

pub(super) async fn rebuild_thread_history_messages_with_retrieval(
    storage: &dyn swage_storage::store::Storage,
    thread_id: Uuid,
    retrieval: Option<swage_core::ReplayRetrieval>,
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
            )?);
        }
    }

    Ok(rebuilt)
}

#[cfg(test)]
pub(super) fn compact_tool_replay_messages(
    records: &[ToolCallRecord],
) -> Result<Vec<ChatMessage>, swage_core::StorageError> {
    compact_tool_replay_messages_with_retrieval(records, None)
}

fn compact_tool_replay_messages_with_retrieval(
    records: &[ToolCallRecord],
    retrieval: Option<swage_core::ReplayRetrieval>,
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
            // A started row proves the model selected the call, even when
            // settlement failed. Describe the unknown outcome without
            // writing a second record or exposing an unscanned raw result.
            let model_result = crate::tool::tool_result_read::recovered_model_result(record)
                .or_else(|| record.model_result.clone())
                .unwrap_or_else(|| {
                    swage_core::compact_tool_result_for_model_with_reference(
                        swage_storage::store::INTERRUPTED_TOOL_RESULT,
                        true,
                        swage_core::tool_result_reference(
                            &record.turn_id.to_string(),
                            &record.call_id,
                        ),
                    )
                });
            let model_result = match (
                retrieval,
                serde_json::from_str::<swage_core::CompactToolReplay>(&model_result),
            ) {
                (Some(retrieval), Ok(replay)) => {
                    replay.with_elision_note(retrieval).to_stable_json()
                }
                _ => model_result,
            };
            tool_calls.push(ToolCall {
                call_id: record.call_id.clone(),
                name: record.tool.clone(),
                arguments: parse_model_arguments(record)?,
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

pub(super) fn parse_model_arguments(
    record: &ToolCallRecord,
) -> Result<serde_json::Value, swage_core::StorageError> {
    let model_error = match serde_json::from_str::<serde_json::Value>(&record.model_arguments) {
        Ok(arguments) if !is_tool_argument_envelope(&arguments) => return Ok(arguments),
        Ok(_) => "stored arguments use a replay envelope".to_string(),
        Err(error) => error.to_string(),
    };
    let arguments = serde_json::from_str(&record.arguments).map_err(|raw_error| {
        swage_core::StorageError::Corruption {
            message: format!(
                "tool call '{}' has unusable model arguments ({model_error}) and invalid raw arguments ({raw_error})",
                record.call_id
            ),
        }
    })?;
    tracing::warn!(
        turn_id = %record.turn_id,
        call_id = %record.call_id,
        error = %model_error,
        "model tool arguments are unusable; replaying bounded raw arguments"
    );
    // Recover the row's original parameters, never a preview that may end
    // mid-JSON, using the same bound as newly stored model arguments.
    Ok(
        serde_json::from_str(&swage_core::compact_tool_arguments_for_model(&arguments))
            .expect("compacted tool arguments are valid JSON"),
    )
}

fn is_tool_argument_envelope(arguments: &serde_json::Value) -> bool {
    arguments.get("kind").and_then(serde_json::Value::as_str) == Some("tool_arguments")
        && ["bytes", "sha256", "truncated", "preview"]
            .iter()
            .all(|key| arguments.get(key).is_some())
}
