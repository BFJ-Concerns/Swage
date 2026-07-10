use super::*;

pub(super) async fn rebuild_thread_history_messages(
    storage: &dyn swage_storage::store::Storage,
    thread_id: Uuid,
) -> Result<Vec<ChatMessage>, GatewayError> {
    let stored_history = storage.list_messages_by_thread(thread_id).await?;
    let mut rebuilt = Vec::new();
    let mut replay_inserted_for_turn = HashSet::new();

    for message in stored_history {
        if message.role == Role::Assistant
            && let Some(turn_id) = message.turn_id
            && replay_inserted_for_turn.insert(turn_id)
        {
            let tool_calls = storage.list_tool_calls_by_turn(turn_id).await?;
            rebuilt.extend(compact_tool_replay_messages(&tool_calls));
        }

        match message.role {
            Role::Tool => {
                if let Some(call_id) = message.tool_call_id {
                    rebuilt.push(ChatMessage::tool_result(call_id, message.content));
                }
            }
            role => rebuilt.push(ChatMessage::new(role, message.content)),
        }
    }

    Ok(rebuilt)
}

pub(super) fn compact_tool_replay_messages(records: &[ToolCallRecord]) -> Vec<ChatMessage> {
    let parent_rounds: HashMap<&str, i32> = records
        .iter()
        .filter(|record| record.parent_call_id.is_none())
        .map(|record| (record.call_id.as_str(), record.provider_round))
        .collect();
    let mut records_by_round: BTreeMap<i32, Vec<&ToolCallRecord>> = BTreeMap::new();

    for record in records
        .iter()
        .filter(|record| record.model_result.is_some())
    {
        let effective_round = record
            .parent_call_id
            .as_deref()
            .and_then(|parent_call_id| parent_rounds.get(parent_call_id))
            .copied()
            .unwrap_or(record.provider_round);
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
            tool_calls.push(ToolCall {
                call_id: record.call_id.clone(),
                name: record.tool.clone(),
                arguments: parse_model_arguments(record),
            });
            tool_results.push(ToolResult {
                call_id: record.call_id.clone(),
                content: record
                    .model_result
                    .clone()
                    .unwrap_or_else(|| record.result.clone().unwrap_or_default()),
                is_error: record.status.as_deref() != Some("success"),
            });
        }

        if !tool_calls.is_empty() {
            replay_messages.extend(build_tool_round_messages(&tool_calls, &tool_results, ""));
        }
    }

    replay_messages
}

pub(super) fn parse_model_arguments(record: &ToolCallRecord) -> serde_json::Value {
    serde_json::from_str(&record.model_arguments).unwrap_or_else(|_| {
        serde_json::from_str(&record.arguments)
            .unwrap_or_else(|_| serde_json::Value::String(record.arguments.clone()))
    })
}
