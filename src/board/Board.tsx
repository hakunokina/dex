import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { AGENTS } from "../agents";
import { openPanel } from "../layout/panels";
import { getActiveTeamId, useTeams } from "../team/store";
import { isAssignable } from "../team/rolePolicy";
import { hostOf } from "../api/hosts";
import { useDelegations } from "../delegations";
import { useTeamCost } from "../usage/costStore";
import { costBreakdown, costText, sumDelegations } from "../usage/costFormat";
import type { UsageTotal } from "../usage/costTypes";
import { createTask, deleteTask, listTasks, onTasksChanged, updateTask, type Task, type TaskPatch, type TaskStatus } from "./api";
import { closeBoard, useBoardOpen } from "./store";
import { setPaletteOpen } from "../palette/store";
import { EmptyState } from "../EmptyState";
import { useCountUp } from "../motion";
import "./board.css";
import { jobProjectName, useTeamProjects } from "../project/store";
import "../project/project.css";
import { PanelsTopLeft, Trash2, X } from "lucide-react";
import { taskRoleName } from "./taskRole";

const COLUMNS: { status: TaskStatus; label: string }[] = [
  { status: "todo", label: "Yapılacak" },
  { status: "doing", label: "Sürüyor" },
  { status: "done", label: "Bitti" },
];

// ---- open/close state lives in ./store (the topbar button must not pull this module) ----
export { openBoard } from "./store";

type Drag = { task: Task; x: number; y: number; dx: number; dy: number; w: number; over: TaskStatus | null };

/** The drawer over the stage (topbar button, `). Its body is the same `BoardView` a dock panel shows (DEX-36). */
export function Board() {
  const isOpen = useBoardOpen();

  return (
    <aside className={`board${isOpen ? " open" : ""}`} aria-hidden={!isOpen} inert={!isOpen}>
      <BoardView
        tools={
          <>
            <button className="icon-btn" onClick={() => {
              closeBoard();
              openPanel(getActiveTeamId(), "board");
            }} title="Panele al (sürükle, böl, yüzdür)">
              <PanelsTopLeft />
            </button>
            <button className="icon-btn" onClick={closeBoard} title="Kapat (Esc)">
              <X />
            </button>
          </>
        }
      />
    </aside>
  );
}

/** Task columns + quick add; in the drawer or as the `board` dock panel. */
export function BoardView({ tools }: { tools?: ReactNode }) {
  // null until the first list lands: the columns show skeleton cards meanwhile.
  const [tasks, setTasks] = useState<Task[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [editing, setEditing] = useState<number | null>(null);
  const [drag, setDrag] = useState<Drag | null>(null);
  const press = useRef<{ task: Task; x: number; y: number; rect: DOMRect } | null>(null);
  const addRef = useRef<HTMLInputElement>(null);
  // #209: işin maliyeti = görevine bağlı devretmelerin toplamı (takım özeti, tüm zamanlar).
  const { activeId: teamId, roles } = useTeams();
  useTeamProjects(teamId); // the cards' project chips follow the team's project list
  const cost = useTeamCost(teamId || null);
  const delegations = useDelegations();
  const taskCost = (t: Task) => {
    if (cost?.status !== "ready") return null;
    const host = hostOf(teamId);
    const ids = delegations.filter((d) => d.taskId === t.id && d.host === host).map((d) => d.id);
    const total = ids.length ? sumDelegations(ids, cost.all) : null;
    return total && { total, warnings: cost.all!.warnings };
  };

  const reload = useCallback(() => {
    listTasks().then(setTasks, (e) => setError(String(e)));
  }, []);

  useEffect(() => {
    reload();
    const un = onTasksChanged(reload);
    return () => void un.then((f) => f());
  }, [reload]);

  const run = (p: Promise<unknown>) => p.then(() => setError(null), (e) => (setError(String(e)), reload()));

  const patch = (t: Task, p: TaskPatch) => {
    setTasks((ts) => ts && ts.map((x) => (x.id === t.id ? { ...x, ...p } : x)));
    run(updateTask(t.id, p));
  };

  const add = () => {
    const title = draft.trim();
    if (!title) return;
    setDraft("");
    run(createTask({ title }));
  };

  const remove = (t: Task) => {
    setTasks((ts) => ts && ts.filter((x) => x.id !== t.id));
    run(deleteTask(t.id));
  };

  // ---- pointer drag (HTML5 DnD is unreliable in WebView2 while Tauri's file-drop is on) ----
  const columnAt = (x: number, y: number) =>
    ((document.elementFromPoint(x, y) as HTMLElement | null)?.closest("[data-status]") as HTMLElement | null)?.dataset
      .status as TaskStatus | undefined;

  const onPointerDown = (e: React.PointerEvent<HTMLDivElement>, task: Task) => {
    if (e.button !== 0 || (e.target as HTMLElement).closest("input,textarea,select,button")) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    press.current = { task, x: e.clientX, y: e.clientY, rect: e.currentTarget.getBoundingClientRect() };
  };

  const onPointerMove = (e: React.PointerEvent) => {
    const p = press.current;
    if (!p) return;
    if (!drag && Math.hypot(e.clientX - p.x, e.clientY - p.y) < 4) return;
    setDrag({
      task: p.task,
      x: e.clientX,
      y: e.clientY,
      dx: p.x - p.rect.left,
      dy: p.y - p.rect.top,
      w: p.rect.width,
      over: columnAt(e.clientX, e.clientY) ?? null,
    });
  };

  const onPointerUp = (e: React.PointerEvent) => {
    const p = press.current;
    press.current = null;
    if (!p) return;
    if (!drag) {
      setEditing((id) => (id === p.task.id ? null : p.task.id));
      return;
    }
    setDrag(null);
    const to = columnAt(e.clientX, e.clientY);
    if (to && to !== p.task.status) patch(p.task, { status: to });
  };

  const cancelDrag = () => {
    press.current = null;
    setDrag(null);
  };

  return (
    <>
      <div className="board-head">
        <span className="board-title">Görevler</span>
        <input
          ref={addRef}
          className="board-add"
          placeholder="Yeni görev… (Enter)"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => e.key === "Enter" && add()}
        />
        {tools}
      </div>
      {error && <div className="board-error">{error}</div>}
      {tasks?.length === 0 ? (
        <div className="board-empty">
          <EmptyState
            mood="board"
            title="Panoda iş yok"
            actions={
              <>
                <button className="primary" onClick={() => addRef.current?.focus()}>İş ver</button>
                <button onClick={() => setPaletteOpen(true)}>Takımları gör</button>
              </>
            }
          >
            Core bekliyor. İlk işi ver ya da bir takıma göz at — burası dolunca kartlar dizilecek.
          </EmptyState>
        </div>
      ) : (
        <div className="board-cols" aria-busy={tasks === null && !error}>
          {COLUMNS.map((c) => {
            const items = tasks?.filter((t) => t.status === c.status) ?? [];
            return (
              <section key={c.status} data-status={c.status} className={`board-col${drag?.over === c.status ? " over" : ""}`}>
                <header>
                  {c.label} <Count n={items.length} />
                </header>
                <div className="board-list">
                  {tasks === null && !error && <SkeletonCards />}
                  {items.map((t) => (
                    <Card
                      key={t.id}
                      task={t}
                      cost={taskCost(t)}
                      role={taskRoleName(t, hostOf(teamId), delegations, roles)}
                      editing={editing === t.id}
                      dragging={drag?.task.id === t.id}
                      onPatch={(p) => patch(t, p)}
                      onDelete={() => remove(t)}
                      onPointerDown={(e) => onPointerDown(e, t)}
                      onPointerMove={onPointerMove}
                      onPointerUp={onPointerUp}
                      onPointerCancel={cancelDrag}
                    />
                  ))}
                </div>
              </section>
            );
          })}
        </div>
      )}
      {drag && (
        <div className="board-card ghost" style={{ left: drag.x - drag.dx, top: drag.y - drag.dy, width: drag.w }}>
          <div className="board-card-title">{drag.task.title}</div>
        </div>
      )}
    </>
  );
}

/** Column count that counts to its new value (Modern §2). */
function Count({ n }: { n: number }) {
  return <b>{useCountUp(n)}</b>;
}

/** Two card-sized placeholders per column while the first list loads. */
function SkeletonCards() {
  return (
    <>
      {[70, 45].map((w) => (
        <div key={w} className="board-card skel-card" aria-hidden="true">
          <div className="skel" style={{ height: 12, width: `${w}%` }} />
          <div className="skel" style={{ height: 22, width: 110, marginTop: 8 }} />
        </div>
      ))}
    </>
  );
}

interface CardProps extends Pick<React.HTMLAttributes<HTMLDivElement>, "onPointerDown" | "onPointerMove" | "onPointerUp" | "onPointerCancel"> {
  task: Task;
  role: string | null;
  cost: { total: UsageTotal; warnings: string[] } | null;
  editing: boolean;
  dragging: boolean;
  onPatch: (p: TaskPatch) => void;
  onDelete: () => void;
}

function Card({ task, role, cost, editing, dragging, onPatch, onDelete, ...pointer }: CardProps) {
  const members = useTeams().agents;
  const assignable = AGENTS.filter(a => {
    const member = members.find(m => m.id === a.id);
    return member && isAssignable(member);
  });
  const agent = AGENTS.find((a) => a.id === task.agentId);
  const project = jobProjectName(task); // #838: only where the team has several projects
  return (
    <div className={`board-card${dragging ? " dragging" : ""}${editing ? " editing" : ""}`} {...pointer}>
      {editing ? (
        <input
          className="board-card-title-input"
          defaultValue={task.title}
          autoFocus
          onBlur={(e) => e.target.value.trim() && e.target.value !== task.title && onPatch({ title: e.target.value })}
          onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
        />
      ) : (
        <div className="board-card-title">{task.title}</div>
      )}
      {editing && (
        <textarea
          className="board-card-desc"
          placeholder="Açıklama…"
          defaultValue={task.description}
          rows={3}
          onBlur={(e) => e.target.value !== task.description && onPatch({ description: e.target.value })}
        />
      )}
      {!editing && task.description && <div className="board-card-desc-view">{task.description}</div>}
      <div className="board-card-foot">
        <select
          className="board-agent"
          value={task.agentId ?? ""}
          onChange={(e) => onPatch({ agentId: e.target.value || null })}
          style={agent ? { borderLeftColor: agent.shirt } : undefined}
        >
          <option value="">— atanmadı</option>
          {agent && !assignable.some(a => a.id === agent.id) && <option value={agent.id} disabled>{agent.name}</option>}
          {assignable.map((a) => (
            <option key={a.id} value={a.id}>
              {a.name}
            </option>
          ))}
        </select>
        {role && <span className="project-chip board-role-chip" title={`rol: ${role}`}>rol: {role}</span>}
        {project && <span className="project-chip" title="Görevin projesi">{project}</span>}
        {cost && (
          <span className="board-cost" tabIndex={0} title={costBreakdown(cost.total, cost.warnings)} aria-label={`İşin maliyeti: ${costBreakdown(cost.total, cost.warnings)}`}>
            {costText(cost.total)}
          </span>
        )}
        <span className="spacer" />
        <button className="icon-btn" onClick={onDelete} title="Sil">
          <Trash2 />
        </button>
      </div>
    </div>
  );
}
