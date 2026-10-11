import { describe, it } from "node:test";
import assert from "node:assert/strict";
import { workerProjectId, workerProjectTeam, blockedJob, projectState, showsProjects, projectErrorText, isUnknownMethod, folderName, leaderProjectMismatch, memoryRequest, memoryProjectId, projectCreateParams, type Project } from "./model.ts";

const project = (id: string, extra: Partial<Project> = {}): Project => ({
  id, host: "local", teamId: "t", name: id, rootPath: "/repo", rootKey: "/repo", repoKey: "git", defaultBranch: "main",
  worktrees: true, archivedAt: null, createdAt: 0, updatedAt: 0, worktreeCleanup: 0, ...extra,
});
const jobs = [{ id: 1, toAgent: "a", status: "running", projectId: "p1" }, { id: 2, toAgent: "a", status: "queued", projectId: "p2" }];
describe("proje göstergesi", () => {
  it("servis worker'ının proje sahibi oturumun işidir; sıradaki iş değildir", () => {
    const serviceJobs = [
      { id: 7, toAgent: "s", status: "done", projectId: "p1", teamId: "main" },
      { id: 8, toAgent: "s", status: "queued", projectId: "p2", teamId: "other" },
    ];
    assert.equal(workerProjectTeam({ projectId: "p1", delegationId: 7 }, serviceJobs, "s", "service"), "main");
    assert.equal(workerProjectTeam(undefined, serviceJobs, "s", "service"), "service");
  });
  it("oturum bağı işten önce gelir; kuyruk başlık değildir", () => {
    assert.equal(workerProjectId({ projectId: "remote:p1", delegationId: 1 }, jobs, "a"), "remote:p1");
    assert.equal(workerProjectId(undefined, jobs, "a"), "p1");
    assert.equal(workerProjectId(undefined, jobs.slice(1), "a"), null);
  });
  it("bağsız mevcut oturuma başka running işin projesi yazılmaz", () => {
    assert.equal(workerProjectId({ projectId: null, delegationId: null }, jobs, "a"), null);
  });
  it("yalnız canlı oturumda FIFO başındaki farklı proje bekler", () => {
    const binding = { projectId: "p1", delegationId: 1 };
    assert.equal(blockedJob(binding, jobs, "a", true)?.id, 2);
    assert.equal(blockedJob(binding, jobs, "a", false), null);
    assert.equal(blockedJob(binding, [...jobs, { id: 0, toAgent: "a", status: "queued", projectId: "p1" }], "a", true), null);
  });
  it("lider yalnız canlı ve seçimi farklıysa yeniden başlatma göstergesi alır", () => {
    assert.equal(leaderProjectMismatch({ projectId: "p1", delegationId: null }, "p2", true), true);
    assert.equal(leaderProjectMismatch({ projectId: null, delegationId: null }, "p2", true), true);
    assert.equal(leaderProjectMismatch(undefined, "p2", true), false);
    assert.equal(leaderProjectMismatch({ projectId: "p1", delegationId: null }, "p1", true), false);
    assert.equal(leaderProjectMismatch({ projectId: "p1", delegationId: null }, "p2", false), false);
  });
});
describe("proje listesi ve hata", () => {
  it("git, klasör, klasörsüz, unavailable ve arşiv ayrıdır", () => {
    assert.equal(projectState(project("a")), "git");
    assert.equal(projectState(project("a", { repoKey: null })), "folder");
    assert.equal(projectState(project("a", { rootPath: null })), "none");
    assert.equal(projectState(project("a"), "project_unavailable"), "unavailable");
    assert.equal(projectState(project("a"), "project_branch_mismatch"), "branch");
    assert.equal(projectState(project("a", { archivedAt: 1 }), "project_unavailable"), "archived");
  });
  it("tek açık proje eski görünümü korur", () => {
    assert.equal(showsProjects([project("a"), project("b", { archivedAt: 1 })]), false);
    assert.equal(showsProjects([project("a"), project("b")]), true);
  });
  it("fallback yalnız metot kanıtıyla; diğer hatalar görünür", () => {
    assert.equal(isUnknownMethod(new Error("bilinmeyen metot: project_list"), "project_list"), true);
    assert.equal(isUnknownMethod(new Error("bağlantı yok"), "project_list"), false);
    assert.match(projectErrorText("project_busy"), /süren iş/);
    assert.equal(projectErrorText(new Error("sunucu kapalı")), "sunucu kapalı");
    assert.equal(folderName("C:\\repo\\site\\"), "site");
  });
  it("Object prototipindeki adlar proje hata kodu değildir", () => {
    assert.equal(projectErrorText("constructor"), "constructor");
    assert.equal(projectErrorText("toString"), "toString");
  });
});
describe("hafıza çağrısı", () => {
  it("harita bağlamı yalnız aktif listedeki projeyi kabul eder", () => {
    const list = [project("local"), project("remote:p1"), project("archived", { archivedAt: 1 })];
    assert.equal(memoryProjectId(list, "local"), "local");
    assert.equal(memoryProjectId(list, "remote:p1"), "remote:p1");
    assert.equal(memoryProjectId(list, "p1"), null);
    assert.equal(memoryProjectId(list, "archived"), null);
    assert.equal(memoryProjectId(list, "missing"), null);
    assert.equal(memoryProjectId([], "local"), null);
    assert.equal(memoryProjectId(list, null), null);
  });
  it("eski host eski metodu, takım bağlamı takım metodunu kullanır", () => {
    assert.deepEqual(memoryRequest("graph", "t", null, true), { method: "memory_graph", params: { teamId: "t" } });
    assert.deepEqual(memoryRequest("graph", "t", null, false), { method: "team_memory_graph", params: { teamId: "t" } });
  });
  it("tek proje grafiği ve read aynı kapsamı kullanır; nitelikli belge id değişmez", () => {
    assert.deepEqual(memoryRequest("graph", "t", "p1", false), { method: "project_memory_graph", params: { teamId: "t", projectId: "p1" } });
    assert.deepEqual(memoryRequest("read", "t", "p2", false, "project/p2/shared/not"), {
      method: "project_memory_read", params: { teamId: "t", projectId: "p2", id: "project/p2/shared/not" },
    });
  });
});
it("new project folder is created by the backend without a root argument", () => {
  assert.deepEqual(projectCreateParams({ name: "Site" }, { createFolder: true }, "main"), {
    project: { name: "Site" }, createFolder: true, branch: "main",
  });
  assert.deepEqual(projectCreateParams({ name: "Repo" }, { root: "C:\\Repo" }, ""), {
    project: { name: "Repo" }, root: "C:\\Repo", branch: null,
  });
});
