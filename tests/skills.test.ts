import { describe, expect, it } from "bun:test";
import { readFileSync } from "node:fs";

// 스킬은 얇은 진입점이다: 실제 사용법은 `knock guide` 가 갖고 있고, 모델이 스스로
// 부를 수 있어야 한다. 누가 SKILL.md 에 사용법을 다시 쌓거나 자동 호출을 막으면 잡는다.
const frontmatter = (text: string) => text.split("---")[1] ?? "";

describe.each(["knock-annotate", "knock-ask"])("skills/%s/SKILL.md", (name) => {
  const text = readFileSync(`skills/${name}/SKILL.md`, "utf8");
  const meta = frontmatter(text);

  it("names itself and describes when the model should call it", () => {
    expect(meta).toContain(`name: ${name}`);
    expect(meta).toMatch(/description: .*Use when/);
  });

  it("does not block model invocation", () => {
    expect(meta).not.toContain("disable-model-invocation");
  });

  it("injects `knock guide` and tells the model to follow it", () => {
    expect(text).toContain("!`knock guide`");
    expect(text).toContain("run `knock guide` yourself");
  });

  it("runs the user's arguments in the background", () => {
    expect(text).toContain("$ARGUMENTS");
    expect(text).toContain("run_in_background: true");
  });

  it("stays thin", () => {
    expect(text.split("\n").length).toBeLessThan(30);
  });
});
