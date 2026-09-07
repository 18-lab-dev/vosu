
export const SOURCE_REVISION = "1a67fd64cc38fe60306328dde0a429e3211ca4ed";
export const OSU_REVISION = "97e94babde9d5ecbe574ecdd33509db27452cd0f";
export const CALCULATOR_REVISION = "9a22c4c74f8e584fa64133a79a204dd045646bac";
export const CALCULATOR_PACKAGE = "0.5.0-20260702-main.0";
export const ROSU_BASELINE = "v4.0.1 / osu! 28c846b4";

export type Json = null | boolean | number | string | Json[] | { [key: string]: Json };
export type UnknownRecord = Record<string, unknown>;

export function fail(message: string): never {
  throw new Error(message);
}

export function record(value: unknown, label: string): UnknownRecord {
  if (typeof value !== "object" || value === null || Array.isArray(value)) fail(`${label} must be an object`);
  return value as UnknownRecord;
}

export function stringField(value: UnknownRecord, key: string, label: string): string {
  const field = value[key];
  if (typeof field !== "string") fail(`${label}.${key} must be a string`);
  return field;
}

export function arrayField(value: UnknownRecord, key: string, label: string): unknown[] {
  const field = value[key];
  if (!Array.isArray(field)) fail(`${label}.${key} must be an array`);
  return field;
}

export function call(target: unknown, method: string, args: unknown[] = []): unknown {
  const object = record(target, `receiver for ${method}`);
  const candidate = object[method];
  if (typeof candidate !== "function") fail(`oracle binding is missing ${method}()`);
  return Reflect.apply(candidate, target, args) as unknown;
}

export function staticCall(target: unknown, method: string, args: unknown[] = []): unknown {
  if ((typeof target !== "object" || target === null) && typeof target !== "function") {
    return fail(`static receiver for ${method} must be an object or constructor`);
  }
  const candidate = Reflect.get(target, method) as unknown;
  if (typeof candidate !== "function") fail(`oracle binding is missing static ${method}()`);
  return Reflect.apply(candidate, target, args) as unknown;
}

export function jsonValue(value: unknown, label: string): Json {
  const encoded = JSON.stringify(value, (_key: string, nested: unknown): unknown => {
    if (ArrayBuffer.isView(nested)) {
      return Array.from(nested as unknown as ArrayLike<unknown>);
    }
    return nested;
  });
  if (encoded === undefined) fail(`${label} is not JSON serializable`);
  return JSON.parse(encoded) as Json;
}
