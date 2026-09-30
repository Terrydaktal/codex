#!/usr/bin/env bash

set -euo pipefail

CODEX_HOME=${CODEX_HOME:-"${HOME}/.codex"}
RESET_AT_OVERRIDE=${CODEX_WEEKLY_RESET_AT:-}
RESET_AT_FALLBACK="2026-07-14T19:00:00Z"
WEEKLY_CREDIT_ALLOWANCE=2700
ACCOUNT_ID_OVERRIDE=${CODEX_ACCOUNT_ID:-}
DRY_RUN=0
SHOW_ONLY=0

usage() {
	cat <<'EOF'
NAME
    bootstrap-weekly-task-usage.sh - rebuild Codex's weekly model-credit ledger

SYNOPSIS
    bootstrap-weekly-task-usage.sh [OPTIONS]

DESCRIPTION
    Scans active and archived Codex rollout JSONL files, calculates model-credit
    usage from the last detected weekly reset onward using a provisional
    allowance of 2700 credits, and writes the shared ledger read by every TUI session. It
		prefers native token_usage_record entries, which contain the incremental
		usage and stable response ID for each completed server response. It
		deduplicates those IDs across active, archived, resumed, and forked rollouts.
		For older history, it derives deltas from cumulative token_count events and
		uses task summaries only when a rollout has neither response nor token-count
		usage. Compaction markers are reported as an audit count; remote compaction
		usage is included by its native response record. If a service tier is
		present, it is reported, but the calculation remains on the configured base
		model rates.
		The reset is detected from the weekly rate-limit window's reset timestamp,
		with a usage drop and the previous ledger as fallbacks.

OPTIONS
    --codex-home PATH    Codex home containing sessions/ and archived_sessions/.
    --account-id ID      ChatGPT workspace/account ID to reconcile. If omitted,
                         it is read from auth.json when available.
    --reset-at TIME      Explicit reset boundary understood by GNU date. This
                         overrides automatic detection.
    --show-limits, --show
                         Calculate and print the result without writing it.
    --dry-run            Alias for --show-limits.
    -h, --help           Show this help text.

OPERATION
    The ledger is written to an account-scoped task_usage_weekly.<fingerprint>.json
    beneath CODEX_HOME and is updated with an inter-process lock. Codex sessions
    add per-turn deltas to this shared value, so a full rollout scan is only
    needed for bootstrap or reconciliation. Legacy rollout records without an
    account ID are never assigned to a named account.

EXAMPLES
    scripts/bootstrap-weekly-task-usage.sh
    scripts/bootstrap-weekly-task-usage.sh --codex-home "$HOME/.codex" --dry-run
    scripts/bootstrap-weekly-task-usage.sh --codex-home "$HOME/.codex" --show-limits
    scripts/bootstrap-weekly-task-usage.sh --account-id "workspace-123"
    scripts/bootstrap-weekly-task-usage.sh --reset-at "2026-07-21T19:00:00Z"

FILES
    CODEX_HOME/task_usage_weekly.<account-fingerprint>.json
    CODEX_HOME/task_usage_weekly.<account-fingerprint>.lock
    CODEX_HOME/sessions/**/*.jsonl
    CODEX_HOME/archived_sessions/**/*.jsonl

PATHS
    CODEX_HOME defaults to $HOME/.codex and may be overridden with --codex-home
    or the CODEX_HOME environment variable.

SECURITY NOTES
    The script reads local rollout history and writes only the local ledger. It
    does not make network requests. Keep the ledger permissions private because
    it contains locally calculated usage history.

EXIT STATUS
    0 on success; non-zero if dependencies, rollout parsing, or ledger writing
    fails.

AUTHORS
    Codex contributors
EOF
}

while (($# > 0)); do
	case "$1" in
	--codex-home)
		[[ $# -ge 2 ]] || {
			echo "--codex-home requires a path" >&2
			exit 2
		}
		CODEX_HOME=$2
		shift 2
		;;
	--account-id)
		[[ $# -ge 2 ]] || {
			echo "--account-id requires an ID" >&2
			exit 2
		}
		ACCOUNT_ID_OVERRIDE=$2
		shift 2
		;;
	--reset-at)
		[[ $# -ge 2 ]] || {
			echo "--reset-at requires a time" >&2
			exit 2
		}
		RESET_AT_OVERRIDE=$2
		shift 2
		;;
	--dry-run)
		DRY_RUN=1
		shift
		;;
	--show-limits | --show)
		DRY_RUN=1
		SHOW_ONLY=1
		shift
		;;
	-h | --help)
		usage
		exit 0
		;;
	*)
		echo "unknown option: $1" >&2
		echo "use --help for usage" >&2
		exit 2
		;;
	esac
done

command -v date >/dev/null || {
	echo "date is required" >&2
	exit 1
}
command -v jq >/dev/null || {
	echo "jq is required" >&2
	exit 1
}
command -v sha256sum >/dev/null || {
	echo "sha256sum is required" >&2
	exit 1
}
command -v base64 >/dev/null || {
	echo "base64 is required" >&2
	exit 1
}

api_key=${CODEX_API_KEY:-${OPENAI_API_KEY:-}}
access_token=${CODEX_ACCESS_TOKEN:-}
auth_file_api_key=""
personal_access_token=""
agent_identity_jwt=""

decode_jwt_payload() {
	local token=$1
	local payload=${token#*.}
	payload=${payload%%.*}
	case $((${#payload} % 4)) in
	0) ;;
	2) payload+='==' ;;
	3) payload+='=' ;;
	*) return 1 ;;
	esac
	printf '%s' "$payload" | tr '_-' '/+' | base64 --decode 2>/dev/null
}

account_id_from_jwt() {
	decode_jwt_payload "$1" |
		jq -r '.auth.chatgpt_account_id // .["https://api.openai.com/auth"].chatgpt_account_id // .chatgpt_account_id // .account_id // empty' \
			2>/dev/null || true
}

if [[ -z "$ACCOUNT_ID_OVERRIDE" && -n "$access_token" ]]; then
	ACCOUNT_ID_OVERRIDE=$(account_id_from_jwt "$access_token")
fi
if [[ -z "$ACCOUNT_ID_OVERRIDE" && -f "$CODEX_HOME/auth.json" ]]; then
	ACCOUNT_ID_OVERRIDE=$(
		jq -r '.tokens.account_id // .tokens.id_token.chatgpt_account_id // .agent_identity.account_id // empty' \
			"$CODEX_HOME/auth.json" 2>/dev/null || true
	)
	auth_file_api_key=$(jq -r '.OPENAI_API_KEY // empty' "$CODEX_HOME/auth.json" 2>/dev/null || true)
	personal_access_token=$(jq -r '.personal_access_token // empty' "$CODEX_HOME/auth.json" 2>/dev/null || true)
	agent_identity_jwt=$(jq -r '.agent_identity | strings // empty' "$CODEX_HOME/auth.json" 2>/dev/null || true)
	if [[ -z "$ACCOUNT_ID_OVERRIDE" && -n "$agent_identity_jwt" ]]; then
		ACCOUNT_ID_OVERRIDE=$(account_id_from_jwt "$agent_identity_jwt")
	fi
fi

if [[ -n "$ACCOUNT_ID_OVERRIDE" ]]; then
	account_scope_id="chatgpt:$ACCOUNT_ID_OVERRIDE"
elif [[ -n "$api_key" ]]; then
	api_key_fingerprint=$(printf '%s' "$api_key" | sha256sum | cut -d' ' -f1)
	account_scope_id="api-key:$api_key_fingerprint"
elif [[ -n "$auth_file_api_key" ]]; then
	api_key_fingerprint=$(printf '%s' "$auth_file_api_key" | sha256sum | cut -d' ' -f1)
	account_scope_id="api-key:$api_key_fingerprint"
elif [[ -n "$personal_access_token" ]]; then
	personal_access_token_fingerprint=$(printf '%s' "$personal_access_token" | sha256sum | cut -d' ' -f1)
	account_scope_id="personal-access-token:$personal_access_token_fingerprint"
elif [[ -n "$agent_identity_jwt" ]]; then
	agent_identity_fingerprint=$(printf '%s' "$agent_identity_jwt" | sha256sum | cut -d' ' -f1)
	account_scope_id="agent-token:$agent_identity_fingerprint"
else
	account_scope_id="unknown"
fi
if [[ "$account_scope_id" == "unknown" ]]; then
	account_scope_suffix="unknown"
else
	account_scope_suffix=$(printf '%s' "$account_scope_id" | sha256sum | cut -d' ' -f1)
fi

ledger_path="$CODEX_HOME/task_usage_weekly.${account_scope_suffix}.json"
if [[ "$account_scope_suffix" == "unknown" ]]; then
	ledger_path="$CODEX_HOME/task_usage_weekly.json"
fi
lock_path="${ledger_path%.json}.lock"
if ((!DRY_RUN)); then
	command -v flock >/dev/null || {
		echo "flock is required for shared ledger updates" >&2
		exit 1
	}
	mkdir -p "$CODEX_HOME"
	umask 077
	exec 9>"$lock_path"
	flock -x 9
fi

if command -v fd >/dev/null; then
	rollout_files() {
		local directory=$1
		[[ -d "$directory" ]] || return 0
		fd --hidden --type f --extension jsonl --extension zst --print0 . "$directory"
	}
else
	rollout_files() {
		local directory=$1
		[[ -d "$directory" ]] || return 0
		find "$directory" -type f \( -name '*.jsonl' -o -name '*.jsonl.zst' \) -print0
	}
fi

process_rollout() {
	local path=$1
	printf 'FILE\n'
	if [[ "$path" == *.jsonl.zst ]]; then
		command -v zstdcat >/dev/null || {
			echo "zstdcat is required to read $path" >&2
			return 1
		}
		zstdcat -- "$path" | jq -Rr '
			fromjson? |
	            def epoch:
                (.timestamp // "")
                | sub("\\.[0-9]+Z$"; "Z")
                | try fromdateiso8601 catch 0;
	            if .type == "turn_context" then
	                ["START", (epoch | tostring), (.payload.turn_id // ""), (.payload.model // "")] | @tsv
			elif .type == "token_usage_record" then
				["RESP", (epoch | tostring), (.payload.response_id // ""),
				 (.payload.model // ""), (.payload.service_tier // ""),
				 (.payload.usage.input_tokens // 0),
				 (.payload.usage.cached_input_tokens // 0),
				 (.payload.usage.output_tokens // 0),
				 (.payload.usage.reasoning_output_tokens // 0),
				 (.payload.account_id // ""), (.payload.turn_id // "")] | @tsv
			elif .type == "event_msg" and .payload.type == "token_count" then
				([
					.payload.rate_limits.primary,
					.payload.rate_limits.secondary
				] | map(select(. != null and .window_minutes == 10080)) | .[0]) as $weekly
				| ["TOK", (epoch | tostring),
					 (.payload.model // ""),
					 (.payload.service_tier // ""),
					 (.payload.info.total_token_usage.input_tokens // 0),
					 (.payload.info.total_token_usage.cached_input_tokens // 0),
					 (.payload.info.total_token_usage.output_tokens // 0),
	                 (.payload.info.total_token_usage.reasoning_output_tokens // 0),
	                 ($weekly.used_percent // ""),
                 ($weekly.resets_at // ""),
                 (.payload.account_id // "")] | @tsv
            elif .type == "event_msg" and .payload.type == "task_usage_summary" then
                ["SUM", (epoch | tostring), (.payload.model // ""),
                 (.payload.weeklyLimitUsedPercent
                  // .payload.weekly_limit_used_percent // ""),
                 (.payload.account_id // "")] | @tsv
	            elif .type == "compacted" then
                ["COMPACT", (epoch | tostring)] | @tsv
            else empty end
        '
	else
		jq -Rr '
			fromjson? |
	            def epoch:
                (.timestamp // "")
                | sub("\\.[0-9]+Z$"; "Z")
                | try fromdateiso8601 catch 0;
	            if .type == "turn_context" then
	                ["START", (epoch | tostring), (.payload.turn_id // ""), (.payload.model // "")] | @tsv
			elif .type == "token_usage_record" then
				["RESP", (epoch | tostring), (.payload.response_id // ""),
				 (.payload.model // ""), (.payload.service_tier // ""),
				 (.payload.usage.input_tokens // 0),
				 (.payload.usage.cached_input_tokens // 0),
				 (.payload.usage.output_tokens // 0),
				 (.payload.usage.reasoning_output_tokens // 0),
				 (.payload.account_id // ""), (.payload.turn_id // "")] | @tsv
			elif .type == "event_msg" and .payload.type == "token_count" then
				([
					.payload.rate_limits.primary,
					.payload.rate_limits.secondary
				] | map(select(. != null and .window_minutes == 10080)) | .[0]) as $weekly
				| ["TOK", (epoch | tostring),
					 (.payload.model // ""),
					 (.payload.service_tier // ""),
					 (.payload.info.total_token_usage.input_tokens // 0),
					 (.payload.info.total_token_usage.cached_input_tokens // 0),
					 (.payload.info.total_token_usage.output_tokens // 0),
	                 (.payload.info.total_token_usage.reasoning_output_tokens // 0),
	                 ($weekly.used_percent // ""),
                 ($weekly.resets_at // ""),
                 (.payload.account_id // "")] | @tsv
            elif .type == "event_msg" and .payload.type == "task_usage_summary" then
                ["SUM", (epoch | tostring), (.payload.model // ""),
                 (.payload.weeklyLimitUsedPercent
                  // .payload.weekly_limit_used_percent // ""),
                 (.payload.account_id // "")] | @tsv
	            elif .type == "compacted" then
                ["COMPACT", (epoch | tostring)] | @tsv
            else empty end
        ' "$path"
	fi
}

event_stream=$(mktemp "${TMPDIR:-/tmp}/bootstrap-weekly-task-usage.XXXXXX")
temporary_ledger=""
cleanup() {
	if [[ -n "$event_stream" ]]; then
		rm -f -- "$event_stream" || true
	fi
	if [[ -n "$temporary_ledger" ]]; then
		rm -f -- "$temporary_ledger" || true
	fi
}
trap cleanup EXIT

{
	for directory in "$CODEX_HOME/sessions" "$CODEX_HOME/archived_sessions"; do
		while IFS= read -r -d '' path; do
			process_rollout "$path"
		done < <(rollout_files "$directory")
	done
} >"$event_stream"

existing_reset_epoch=""
if [[ -f "$ledger_path" ]]; then
	existing_scope_id=$(jq -r '.account_scope_id // "unknown"' "$ledger_path" 2>/dev/null || echo unknown)
	if [[ "$existing_scope_id" != "$account_scope_id" ]]; then
		existing_scope_id=""
	else
		existing_reset_epoch=$(jq -r '.reset_at_unix_seconds // empty' "$ledger_path" 2>/dev/null || true)
	fi
	if [[ ! "$existing_reset_epoch" =~ ^[0-9]+$ ]]; then
		existing_reset_epoch=""
	fi
fi

now_epoch=$(date +%s)
reset_source=""
reset_details=""
if [[ -n "$RESET_AT_OVERRIDE" ]]; then
	reset_epoch=$(date -d "$RESET_AT_OVERRIDE" +%s) || {
		echo "could not parse reset time: $RESET_AT_OVERRIDE" >&2
		exit 2
	}
	reset_source="explicit override"
	reset_details="$RESET_AT_OVERRIDE"
else
	schedule_detection=$(awk -F '\t' -v now_epoch="$now_epoch" -v target_account_id="$ACCOUNT_ID_OVERRIDE" '
		$1 == "TOK" &&
			(target_account_id == "" ? $11 == "" : $11 == target_account_id) &&
			$10 ~ /^[0-9]+$/ && ($10 + 0) > 0 {
			candidate = ($10 + 0) - 10080 * 60
			if (candidate <= now_epoch && candidate > latest_reset) {
				latest_reset = candidate
				next_reset = $10 + 0
			}
		}
		END {
			if (latest_reset > 0) printf "%.0f\t%.0f\n", latest_reset, next_reset
		}
	' "$event_stream")
	if [[ -n "$schedule_detection" ]]; then
		IFS=$'\t' read -r reset_epoch next_reset_epoch <<<"$schedule_detection"
		reset_source="weekly rate-limit snapshot"
		reset_details="next reset $(date -u -d "@$next_reset_epoch" '+%Y-%m-%dT%H:%M:%SZ') minus 7 days"
	else
		usage_drop_detection=$(
			awk -F '\t' -v target_account_id="$ACCOUNT_ID_OVERRIDE" \
				'$1 == "TOK" &&
					(target_account_id == "" ? $11 == "" : $11 == target_account_id) &&
					$9 != "" { print $2 "\t" $9 }' "$event_stream" |
				sort -n -k1,1 |
				awk -F '\t' '
					{
						timestamp = $1 + 0
						used = $2 + 0
						if (have_previous && timestamp > previous_timestamp && previous_used >= 50 && used <= 1) {
							latest_reset = timestamp
						}
						if (!have_previous || timestamp >= previous_timestamp) {
							previous_timestamp = timestamp
							previous_used = used
							have_previous = 1
						}
					}
					END {
						if (latest_reset > 0) printf "%.0f\n", latest_reset
					}
				'
		)
		if [[ -n "$usage_drop_detection" ]]; then
			reset_epoch=$usage_drop_detection
			reset_source="weekly usage drop"
			reset_details="detected at $(date -u -d "@$reset_epoch" '+%Y-%m-%dT%H:%M:%SZ')"
		elif [[ -n "$existing_reset_epoch" ]]; then
			reset_epoch=$existing_reset_epoch
			reset_source="previous ledger"
			reset_details="no newer reset was present in rollout snapshots"
		else
			reset_epoch=$(date -d "$RESET_AT_FALLBACK" +%s) || {
				echo "could not parse fallback reset time: $RESET_AT_FALLBACK" >&2
				exit 2
			}
			reset_source="historical fallback"
			reset_details="no reset metadata was present in rollout snapshots"
		fi
	fi
fi

RESET_AT=$(date -u -d "@$reset_epoch" '+%Y-%m-%dT%H:%M:%SZ')

metrics=$(
	awk -F '\t' -v reset_epoch="$reset_epoch" \
		-v target_account_id="$ACCOUNT_ID_OVERRIDE" \
		-v credit_denominator="$((10000 * WEEKLY_CREDIT_ALLOWANCE))" '
		function account_matches(event_account_id) {
			if (target_account_id == "") return event_account_id == ""
			return event_account_id == target_account_id
		}
		function add_usage(response_model, current_input, current_cached, current_output, current_reasoning,
			uncached_delta, uncached_rate, cached_rate, output_rate, cost) {
			if (response_model == "") response_model = "<missing model>"
			uncached_delta = current_input - current_cached
			if (uncached_delta < 0) uncached_delta = 0
			input_tokens += current_input
			cached_input_tokens += current_cached
			output_tokens += current_output
			reasoning_tokens += current_reasoning
			model_input[response_model] += current_input
			model_cached[response_model] += current_cached
			model_output[response_model] += current_output
			model_reasoning[response_model] += current_reasoning
			uncached_rate = model_rate(response_model, "uncached")
			cached_rate = model_rate(response_model, "cached")
			output_rate = model_rate(response_model, "output")
			if (uncached_rate == 0) {
				unknown[response_model] = 1
				return
			}
			cost = (uncached_rate * uncached_delta + cached_rate * current_cached + output_rate * current_output) / credit_denominator
			raw_used += cost
			model_used[response_model] += cost
		}
		function finalize_file() {
			if (account_matches(file_account_id)) {
				for (turn_model in file_turn_models) {
					model_turns[turn_model] += file_turn_models[turn_model]
				}
				compaction_count += file_compaction_count
			}
			if (file_usage_record_count == 0 && file_summary_count > 0) {
				raw_used += file_summary_used
			}
		}
        function delta(current, previous) {
            if (!have_previous) return current
            if (current < previous) return 0
            return current - previous
        }
	        function model_rate(model, field) {
	            model = tolower(model)
	            if (model ~ /gpt-5\.4-mini/) {
                if (field == "uncached") return 18.75
	                if (field == "cached") return 1.875
	                return 113
	            }
	            if (model ~ /gpt-6-astra/) {
	                if (field == "uncached") return 250
	                if (field == "cached") return 25
	                return 1250
	            }
	            if (model ~ /gpt-6\.1-sol/) {
	                if (field == "uncached") return 50
	                if (field == "cached") return 2.5
	                return 250
	            }
	            if (model ~ /gpt-6-sol/) {
	                if (field == "uncached") return 50
	                if (field == "cached") return 5
	                return 250
	            }
	            if (model ~ /gpt-6-luna/) {
	                if (field == "uncached") return 2.5
	                if (field == "cached") return 0.25
	                return 12.5
	            }
	            if (model ~ /gpt-5\.6-sol/) {
	                if (field == "uncached") return 100
	                if (field == "cached") return 10
	                return 500
            }
            if (model ~ /gpt-5\.6-terra/ || model ~ /gpt-5\.4/) {
                if (field == "uncached") return 62.5
                if (field == "cached") return 6.25
                return 375
	            }
	            if (model ~ /gpt-5\.6-luna/) {
	                if (field == "uncached") return 5
	                if (field == "cached") return 0.5
	                return 30
            }
            return 0
        }
		$1 == "FILE" {
			if (have_file) finalize_file()
			have_file = 1
			model = ""
			service_tier = ""
			file_summary_used = 0
			file_summary_count = 0
			file_usage_record_count = 0
			for (turn_model in file_turn_models) delete file_turn_models[turn_model]
			file_compaction_count = 0
			file_account_id = ""
			pending_response_record = 0
			have_previous = 0
			previous_input = previous_cached = previous_output = previous_reasoning = 0
			next
		}
		$1 == "START" {
			model = $4
			service_tier = ""
			if (($2 + 0) >= reset_epoch && model != "") file_turn_models[model]++
			next
		}
		$1 == "SUM" {
			event_account_id = $5
			if (!account_matches(event_account_id)) next
			if (event_account_id != "") file_account_id = event_account_id
			if (($2 + 0) >= reset_epoch && $4 != "") {
				file_summary_used += $4
				file_summary_count++
				summary_count++
			}
			next
		}
		$1 == "RESP" {
			timestamp = $2 + 0
			response_id = $3
			response_model = $4 != "" ? $4 : model
			response_service_tier = $5 != "" ? $5 : service_tier
			event_account_id = $10
			if (!account_matches(event_account_id)) next
			if (event_account_id != "") file_account_id = event_account_id
			pending_response_record = 1
			if (timestamp < reset_epoch || response_id == "") next
			if (response_service_tier != "") observed_service_tiers[response_service_tier] = 1
			if (response_id in response_ids) next
			response_ids[response_id] = 1
			response_record_count++
			file_usage_record_count++
			add_usage(response_model, $6 + 0, $7 + 0, $8 + 0, $9 + 0)
			next
		}
		$1 == "COMPACT" {
			if (($2 + 0) >= reset_epoch) file_compaction_count++
			next
		}
		$1 == "TOK" {
			timestamp = $2 + 0
			response_model = $3 != "" ? $3 : model
			response_service_tier = $4 != "" ? $4 : service_tier
			event_account_id = $11
			current_input = $5 + 0
			current_cached = $6 + 0
			current_output = $7 + 0
			current_reasoning = $8 + 0
			input_delta = delta(current_input, previous_input)
			cached_delta = delta(current_cached, previous_cached)
			output_delta = delta(current_output, previous_output)
			reasoning_delta = delta(current_reasoning, previous_reasoning)
			previous_input = current_input
			previous_cached = current_cached
			previous_output = current_output
			previous_reasoning = current_reasoning
			have_previous = 1
			if (!account_matches(event_account_id)) next
			if (event_account_id != "") file_account_id = event_account_id
			if (timestamp < reset_epoch) next
			if (response_service_tier != "") observed_service_tiers[response_service_tier] = 1
			token_event_count++
			if (pending_response_record) {
				pending_response_record = 0
				next
			}
			file_usage_record_count++
			add_usage(response_model, input_delta, cached_delta, output_delta, reasoning_delta)
		}
		END {
			if (have_file) finalize_file()
			used = raw_used
			printf "%.12f\t%.12f\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t%d\t", used, 100 - used,
				input_tokens, cached_input_tokens, output_tokens, reasoning_tokens,
				response_record_count, token_event_count, summary_count, compaction_count
            first = 1
			for (model in unknown) {
				if (!first) printf ","
				printf "%s", model
				first = 0
			}
			printf "\t"
			first = 1
			for (service_tier in observed_service_tiers) {
				if (!first) printf ","
				printf "%s", service_tier
				first = 0
			}
			printf "\n"
			for (model in model_used) {
				printf "MODEL\t%s\t%d\t%.12f\t%d\t%d\t%d\t%d\n", model, model_turns[model] + 0,
					model_used[model] + 0, model_input[model] + 0, model_cached[model] + 0,
					model_output[model] + 0, model_reasoning[model] + 0
			}
			for (model in model_turns) {
				if (!(model in model_used)) {
					printf "MODEL\t%s\t%d\t%.12f\t%d\t%d\t%d\t%d\n", model, model_turns[model], 0,
						0, 0, 0, 0
				}
			}
			for (model in model_input) {
				if (!(model in model_used) && !(model in model_turns)) {
					printf "MODEL\t%s\t%d\t%.12f\t%d\t%d\t%d\t%d\n", model, 0, 0,
						model_input[model], model_cached[model], model_output[model], model_reasoning[model]
				}
			}
		}
	' "$event_stream"
)

IFS=$'\t' read -r used_percent remaining_percent input_tokens cached_input_tokens output_tokens reasoning_tokens response_record_count token_event_count summary_count compaction_count unknown_models observed_service_tiers <<<"$metrics"
model_breakdown=$(printf '%s\n' "$metrics" | tail -n +2 | sort -t $'\t' -k2,2)

printf 'weekly credit allowance: %s credits\n' "$WEEKLY_CREDIT_ALLOWANCE"
if [[ "$account_scope_id" == chatgpt:* ]]; then
	account_scope_display="chatgpt:$(printf '%s' "$account_scope_id" | sed 's/^chatgpt://' | sha256sum | cut -d' ' -f1)"
else
	account_scope_display="$account_scope_id"
fi
printf 'account usage scope: %s\n' "$account_scope_display"
printf 'detected weekly reset: %s (%s; %s)\n' "$RESET_AT" "$reset_source" "$reset_details"
printf 'weekly limit used since %s: %.12f%%\n' "$RESET_AT" "$used_percent"
printf 'weekly limit remaining: %.12f%%\n' "$remaining_percent"
printf 'tokens: input %s (cached %s, non-cached %s) · output %s (reasoning %s)\n' \
	"$input_tokens" "$cached_input_tokens" "$((input_tokens - cached_input_tokens))" \
	"$output_tokens" "$reasoning_tokens"
printf 'records: response usage %s · token_count %s · task summaries %s · compactions %s\n' \
	"$response_record_count" "$token_event_count" "$summary_count" "$compaction_count"
if [[ -n "$model_breakdown" ]]; then
	printf 'model breakdown:\n'
	while IFS=$'\t' read -r record model model_turns used_model_percent input_model cached_model output_model reasoning_model; do
		[[ "$record" == MODEL ]] || continue
		printf '  %s: turns %s · %.12f%% · input %s (cached %s, non-cached %s) · output %s (reasoning %s)\n' \
			"$model" "$model_turns" "$used_model_percent" "$input_model" "$cached_model" "$((input_model - cached_model))" \
			"$output_model" "$reasoning_model"
	done <<<"$model_breakdown"
fi
if [[ -n "$unknown_models" ]]; then
	printf 'warning: skipped unsupported models: %s\n' "$unknown_models" >&2
fi
if [[ -n "$observed_service_tiers" ]]; then
	printf 'warning: observed service tiers: %s; calculation uses base model rates\n' "$observed_service_tiers" >&2
fi

if ((DRY_RUN)); then
	if ((SHOW_ONLY)); then
		printf 'show-only: did not update %s\n' "$ledger_path"
	else
		printf 'dry run: would update %s\n' "$ledger_path"
	fi
	exit 0
fi

temporary_ledger=$(mktemp "$ledger_path.tmp.XXXXXX")

existing_endpoint=""
existing_turn_ids='[]'
existing_response_event_ids='[]'
existing_reset_at=""
if [[ -f "$ledger_path" ]]; then
	existing_scope_id=$(jq -r '.account_scope_id // "unknown"' "$ledger_path" 2>/dev/null || echo unknown)
	if [[ "$existing_scope_id" == "$account_scope_id" ]]; then
		existing_reset_at=$(jq -r '.reset_at_unix_seconds // empty' "$ledger_path" 2>/dev/null || true)
	fi
	if [[ "$existing_reset_at" == "$reset_epoch" ]]; then
		existing_endpoint=$(jq -r '.last_endpoint_remaining_percent // empty' "$ledger_path" 2>/dev/null || true)
		existing_turn_ids=$(jq -c 'if (.applied_turn_ids | type) == "array" then .applied_turn_ids else [] end' "$ledger_path" 2>/dev/null || printf '[]')
		existing_response_event_ids=$(jq -c 'if (.applied_response_event_ids | type) == "array" then .applied_response_event_ids else [] end' "$ledger_path" 2>/dev/null || printf '[]')
	fi
fi

jq -n \
	--argjson reset_at_unix_seconds "$reset_epoch" \
	--arg reset_at "$RESET_AT" \
	--arg reset_source "$reset_source" \
	--arg account_scope_id "$account_scope_id" \
	--arg used_percent "$used_percent" \
	--arg endpoint "$existing_endpoint" \
	--argjson applied_turn_ids "$existing_turn_ids" \
	--argjson applied_response_event_ids "$existing_response_event_ids" \
	--argjson weekly_credit_allowance "$WEEKLY_CREDIT_ALLOWANCE" \
	'{
	        account_scope_id: (if $account_scope_id == "unknown" then null else $account_scope_id end),
	        reset_at_unix_seconds: $reset_at_unix_seconds,
        reset_at: $reset_at,
        reset_detection_source: $reset_source,
        used_percent: ($used_percent | tonumber),
        weekly_credit_allowance: $weekly_credit_allowance,
        last_endpoint_remaining_percent: (if $endpoint == "" then null else ($endpoint | tonumber) end),
        applied_turn_ids: $applied_turn_ids,
        applied_response_event_ids: $applied_response_event_ids
    }' >"$temporary_ledger"
mv -f -- "$temporary_ledger" "$ledger_path"
trap - EXIT
printf 'updated %s\n' "$ledger_path"
