type RoleMember = { id: string; roleSlug: string | null };

/** The patron talks directly to leaders and their personal assistant. */
export const patronWritable = (agent: { kind: string; roleSlug: string | null } | null) =>
  !!agent && (agent.kind === "leader" || agent.roleSlug === "assistant");

export const isAssignable = (agent: { roleSlug: string | null }) => agent.roleSlug !== "assistant";

/** Personal roles enter through member management, never the worker crew picker. */
export function memberRoles<T extends { slug: string; category: string }>(roles: T[], profile: string, allowAssistant: boolean): T[] {
  return roles.filter(r => (r.category === "education") === (profile === "edu") && (allowAssistant || r.slug !== "assistant"));
}

export const assistantTaken = (members: RoleMember[], exceptId?: string) =>
  members.some(a => a.id !== exceptId && a.roleSlug === "assistant");
