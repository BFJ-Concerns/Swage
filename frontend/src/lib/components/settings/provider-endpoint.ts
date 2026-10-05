export function providerBaseUrlError(value: string): string | null {
  return /^https?:\/\//i.test(value.trim())
    ? null
    : "Base URL must start with http:// or https://";
}
