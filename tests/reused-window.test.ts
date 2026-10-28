import { describe, expect, it } from "bun:test";
import { readFileSync } from "node:fs";

// 데몬은 창 하나를 여러 요청에 다시 쓰고, dropStaleListeners() 는 footer 를 class 째로
// 복제한다. 그래서 요청마다 바뀌는 표시는 "붙이기만" 하면 다음 요청까지 남는다.
// 2026-10-28: 비게이트 뷰어 뒤에 뜬 --gate 창에서 승인 카드가 사라져 승인할 수 없었다.
const main = readFileSync("src/main.ts", "utf8");

describe("reused daemon window", () => {
  it("sets the approve card's visibility both ways on every annotate", () => {
    expect(main).toContain('optApprove.classList.toggle("hidden", !p.gate)');
  });

  it("never only adds hidden to the approve card", () => {
    expect(main).not.toMatch(/optApprove\.classList\.add\("hidden"\)/);
    expect(main).not.toMatch(/\$\("opt-approve"\)\.classList\.add\("hidden"\)/);
  });
});
