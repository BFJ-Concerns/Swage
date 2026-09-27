use super::{
    BTreeMap, ChatMessage, GatewayError, HashMap, HashSet, Role, ToolCall, ToolCallRecord,
    ToolResult, Uuid, build_tool_round_messages,
};

pub(super) async fn rebuild_thread_history_messages(
    storage: &dyn swage_storage::store::Storage,
    thread_id: Uuid,
) -> Result<Vec<ChatMessage>, GatewayError> {
    let stored_history = storage.list_messages_by_thread(thread_id).await?;
    let mut rebuilt = Vec::new();
    let mut replay_inserted_for_turn = HashSet::new();

    // A turn's durable tool rounds replay before its assistant message. A turn
    // that never reached one — interrupted mid-tool — still replays its
    // rounds, anchored after its first stored message, so a resumed
    // continuation sees the calls that already ran (and those cut off) instead
    // of selecting them again.
    let turns_with_assistant_message: HashSet<Uuid> = stored_history
        .iter()
        .filter(|message| message.role == Role::Assistant)
        .filter_map(|message| message.turn_id)
        .collect();

    for message in stored_history {
        let replay_turn = message.turn_id.filter(|turn_id| {
            !replay_inserted_for_turn.contains(turn_id)
                && (message.role == Role::Assistant
                    || !turns_with_assistant_message.contains(turn_id))
        });
        let replay_before = replay_turn.filter(|_| message.role == Role::Assistant);
        let replay_after = replay_turn.filter(|_| message.role != Role::Assistant);

        if let Some(turn_id) = replay_before {
            replay_inserted_for_turn.insert(turn_id);
            let tool_calls = storage.list_tool_calls_by_turn(turn_id).await?;
            rebuilt.extend(compact_tool_replay_messages(&tool_calls)?);
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
            rebuilt.extend(compact_tool_replay_messages(&tool_calls)?);
        }
    }

    Ok(rebuilt)
}

pub(super) fn compact_tool_replay_messages(
    records: &[ToolCallRecord],
) -> Result<Vec<ChatMessage>, swage_core::StorageError> {
    let parent_rounds: HashMap<&str, i32> = records
        .iter()
        .filter(|record| record.parent_call_id.is_none())
        .map(|record| (record.call_id.as_str(), record.provider_round))
        .collect();
    let mut records_by_round: BTreeMap<i32, Vec<&ToolCallRecord>> = BTreeMap::new();

    for record in records
        .iter()
        .filter(|record| record.model_result.is_some() && !record.tool.trim().is_empty())
    {
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
            // Only completed calls enter `records_by_round`; retaining the
            // guard here prevents a future caller from replaying raw results
            // when the compact, model-safe representation is absent.
            let Some(model_result) = record.model_result.clone() else {
                continue;
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
    match serde_json::from_str(&record.model_arguments) {
        Ok(arguments) => Ok(arguments),
        Err(model_error) => match serde_json::from_str(&record.arguments) {
            Ok(arguments) => {
                // Older rows may predate compact model arguments. The raw
                // arguments are still provider-valid JSON, so using them is
                // an explicit degraded replay rather than invented context.
                tracing::warn!(
                    turn_id = %record.turn_id,
                    call_id = %record.call_id,
                    error = %model_error,
                    "compact tool arguments are invalid; replaying validated raw arguments"
                );
                Ok(arguments)
            }
            Err(raw_error) => Err(swage_core::StorageError::Corruption {
                message: format!(
                    "tool call '{}' has invalid compact arguments ({model_error}) and raw arguments ({raw_error})",
                    record.call_id
                ),
            }),
        },
    }
}
