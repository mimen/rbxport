import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createLink, retryDelay, type Outcome } from "./link";

const refused: Outcome = { ok: false, reason: "refused", message: "Connection refused (os error 61)" };
const ok: Outcome = { ok: true };

/** A link whose checks answer `answers` in turn, the last one forever after. */
function linkAnswering(...answers: Outcome[]) {
  const check = vi.fn(() => Promise.resolve(answers.length > 1 ? answers.shift() as Outcome : answers[0] as Outcome));
  const recovered = vi.fn();
  const link = createLink({ check, recovered });
  return { link, check, recovered };
}

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("the host link", () => {
  it("connects without a reload when the first check passes", async () => {
    const { link, recovered } = linkAnswering(ok);
    expect(link.state()).toEqual({ status: "connecting" });
    link.check();
    await vi.advanceTimersByTimeAsync(0);
    expect(link.state()).toEqual({ status: "connected" });
    expect(recovered).not.toHaveBeenCalled();
  });

  it("names the reason a check failed", async () => {
    const { link } = linkAnswering(refused);
    link.check();
    await vi.advanceTimersByTimeAsync(0);
    expect(link.state()).toEqual({ status: "unreachable", reason: "refused", message: refused.message, checking: false });
  });

  it("keeps checking with a backoff, and reloads once when the host answers", async () => {
    const { link, check, recovered } = linkAnswering(refused, refused, refused, ok);
    const seen: string[] = [];
    link.subscribe((state) => seen.push(state.status));
    link.check();
    await vi.advanceTimersByTimeAsync(0);
    expect(check).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(999);
    expect(check).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1);
    expect(check).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(2000);
    expect(check).toHaveBeenCalledTimes(3);
    await vi.advanceTimersByTimeAsync(4000);
    expect(check).toHaveBeenCalledTimes(4);
    expect(link.state()).toEqual({ status: "connected" });
    expect(recovered).toHaveBeenCalledTimes(1);
    expect(seen.at(-1)).toBe("connected");
    await vi.advanceTimersByTimeAsync(60_000);
    expect(check).toHaveBeenCalledTimes(4);
  });

  it("caps the backoff at 15 seconds", () => {
    expect([1, 2, 3, 4, 5, 9].map(retryDelay)).toEqual([1000, 2000, 4000, 8000, 15_000, 15_000]);
  });

  it("shows a check under way, and runs one at a time", async () => {
    let finish: (outcome: Outcome) => void = () => {};
    const check = vi.fn()
      .mockResolvedValueOnce(refused)
      .mockImplementation(() => new Promise<Outcome>((resolve) => { finish = resolve; }));
    const link = createLink({ check, recovered: vi.fn() });
    link.check();
    await vi.advanceTimersByTimeAsync(0);
    link.check();
    link.failed();
    link.check();
    expect(check).toHaveBeenCalledTimes(2);
    expect(link.state()).toMatchObject({ status: "unreachable", checking: true });
    finish({ ok: false, reason: "timedOut", message: "No answer in 3 seconds." });
    await vi.advanceTimersByTimeAsync(0);
    expect(link.state()).toMatchObject({ status: "unreachable", reason: "timedOut", checking: false });
  });

  it("checks after a failed request, and stays connected when the check passes", async () => {
    const { link, check, recovered } = linkAnswering(ok);
    link.check();
    await vi.advanceTimersByTimeAsync(0);
    link.failed();
    link.failed();
    await vi.advanceTimersByTimeAsync(0);
    expect(check).toHaveBeenCalledTimes(2);
    expect(link.state()).toEqual({ status: "connected" });
    expect(recovered).not.toHaveBeenCalled();
  });

  it("leaves a failed request to the backoff while unreachable", async () => {
    const { link, check } = linkAnswering(refused);
    link.check();
    await vi.advanceTimersByTimeAsync(0);
    link.failed();
    link.failed();
    await vi.advanceTimersByTimeAsync(999);
    expect(check).toHaveBeenCalledTimes(1);
  });

  it("recovers on an answered request without waiting for the next check", async () => {
    const { link, check, recovered } = linkAnswering(refused);
    link.check();
    await vi.advanceTimersByTimeAsync(0);
    link.answered();
    expect(link.state()).toEqual({ status: "connected" });
    expect(recovered).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(60_000);
    expect(check).toHaveBeenCalledTimes(1);
  });

  it("counts a check that throws as a failed one", async () => {
    const link = createLink({ check: () => Promise.reject(new Error("boom")), recovered: vi.fn() });
    link.check();
    await vi.advanceTimersByTimeAsync(0);
    expect(link.state()).toMatchObject({ status: "unreachable", reason: "other", message: "Error: boom" });
  });
});
