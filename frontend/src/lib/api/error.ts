// The wire error contract as the frontend reads it.
//
// Every failed gateway request answers `{ code, message }`
// (`ApiErrorBody` in the generated protocol). This error carries that body
// so a caller can branch on `code` and show `message` to a person; a
// response outside the contract (a proxy surface, an empty body) still throws,
// with the status and raw text kept for diagnostics.

import type { ApiErrorBody } from "../protocol/ApiErrorBody";
import type { ErrorCode } from "../protocol/ErrorCode";

export class ApiError extends Error {
  readonly status: number;
  /** The contract's code, or null when the body was not the contract. */
  readonly code: ErrorCode | null;

  constructor(status: number, code: ErrorCode | null, message: string) {
    super(message);
    this.name = "ApiError";
    this.status = status;
    this.code = code;
  }

  /**
   * Read a failed response into an error. The message is the gateway's own
   * when the body is the contract; otherwise the request line and status,
   * followed by whatever the body held.
   */
  static async fromResponse(response: Response, method: string, path: string): Promise<ApiError> {
    const text = await response.text().catch(() => "");
    const body = parseContract(text);
    if (body) {
      return new ApiError(response.status, body.code, body.message);
    }
    const detail = text.trim() ? `: ${text.trim()}` : "";
    return new ApiError(response.status, null, `HTTP ${response.status} from ${method} ${path}${detail}`);
  }

  /** Whether this error carries the given contract code. */
  is(code: ErrorCode): boolean {
    return this.code === code;
  }
}

// The contract's code set, mirrored from the generated `ErrorCode` union so
// a body carrying a code outside it is treated as out of contract, exactly
// as the Rust client does. The `satisfies` clause fails the type-check the
// moment the generated union gains or loses a member.
const ERROR_CODES = [
  "malformed_body",
  "unsupported_media_type",
  "validation_error",
  "forbidden",
  "not_found",
  "conflict",
  "turn_in_progress",
  "already_connected",
  "provider_error",
  "sign_in_unsupported",
  "sign_in_failed",
  "provider_runtime_unavailable",
  "internal_error",
] as const satisfies readonly ErrorCode[];

function isErrorCode(value: unknown): value is ErrorCode {
  return typeof value === "string" && (ERROR_CODES as readonly string[]).includes(value);
}

function parseContract(text: string): ApiErrorBody | null {
  try {
    const value: unknown = JSON.parse(text);
    if (
      typeof value === "object" &&
      value !== null &&
      isErrorCode((value as { code?: unknown }).code) &&
      typeof (value as { message?: unknown }).message === "string"
    ) {
      return value as ApiErrorBody;
    }
  } catch {
    // not JSON — fall through
  }
  return null;
}
