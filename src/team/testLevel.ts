import type { TestLevel } from "./api";

/** Unknown/absent levels mean the host cannot offer this setting. */
export function nextTestLevel(level: unknown): TestLevel | null {
  return level === "full" ? "light" : level === "light" ? "full" : null;
}
