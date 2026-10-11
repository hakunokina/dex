// #212: pane üst çubuğunda güncel context doluluğu — lider: "312k / 1M" + ince çubuk, worker: yalnız sayı.
import { contextFill, contextLevel, contextText, contextTitle } from "./contextFormat";
import type { AgentContext } from "./costTypes";

export function ContextMeter({ ctx, compact = false }: { ctx: AgentContext; compact?: boolean }) {
  const level = ctx.updatedAt === null ? "ok" : contextLevel(ctx.tokens);
  const fill = compact ? null : contextFill(ctx);
  const title = contextTitle(ctx);
  return (
    <span className={`ctx-meter ctx-${level}${compact ? " compact" : ""}`} tabIndex={0} title={title}
      aria-label={`Context doluluğu. ${title}`} data-level={level}>
      <span className="ctx-text">{contextText(ctx, !compact)}</span>
      {fill !== null && (
        <span className="ctx-bar" aria-hidden="true">
          <span style={{ width: `${(fill * 100).toFixed(1)}%` }} />
        </span>
      )}
    </span>
  );
}
