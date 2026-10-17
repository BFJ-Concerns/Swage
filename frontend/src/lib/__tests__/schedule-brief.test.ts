import { describe, expect, it } from "vitest";
import { formatScheduleBrief } from "../schedule-brief";

describe("formatScheduleBrief", () => {
  it("trims thousands of surrounding newlines from a short heading", () => {
    const brief = "Review the inbox.";
    const message = "\n".repeat(3000) + brief + "\n".repeat(3000);

    expect(formatScheduleBrief({ id: "schedule-1", message })).toEqual({
      heading: brief,
      truncated: false,
    });
  });

  it("collapses whitespace runs within a short heading", () => {
    expect(formatScheduleBrief({
      id: "schedule-1",
      message: "  Review\n\n the\t inbox.  ",
    })).toEqual({ heading: "Review the inbox.", truncated: false });
  });
});
