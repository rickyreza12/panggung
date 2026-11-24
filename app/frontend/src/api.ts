export interface User { id: string | number; username: string; role: string }
export interface Component { name: string; namespace: string; owner: string; description: string; component_type: string | null; lifecycle: string | null; descriptor: unknown }
export interface Relation { type: string; targetRef: string; resolved?: boolean }
export interface Domain { namespace: string; name: string }
export interface System extends Domain { domain_namespace: string | null; domain_name: string | null }
export function entityKey(entity: Domain) { return entity.namespace + '/' + entity.name; }
export function relationStatus(relation: Relation) {
  return relation.resolved === true ? 'Resolved' : relation.resolved === false ? 'Unresolved' : 'Not checked';
}
export function structureRequest(kind: 'domains' | 'systems', namespace: string, name: string, domain?: Domain) {
  const valid = (value: string) => /^[a-z0-9](?:[a-z0-9_.-]{0,61}[a-z0-9])?$/.test(value);
  if (!valid(namespace) || !valid(name) || (domain && (!valid(domain.namespace) || !valid(domain.name)))) throw new Error('Invalid namespace or name. Use 1–63 lowercase characters and start and end with a letter or digit.');
  const body = kind === 'domains' ? {namespace,name} : {namespace,name,domain_namespace:domain?.namespace ?? null,domain_name:domain?.name ?? null};
  return {path:'/api/catalog/'+kind, init:json('POST',body)};
}
export interface Fields { name: string; owner: string; description: string }
export interface Filters { q: string; namespace: string }
export class ApiError extends Error {
  constructor(public status: number, message: string) { super(message); this.name = 'ApiError'; }
}
export async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const response = await fetch(path, { ...init, credentials: 'same-origin' });
  if (!response.ok) {
    let message = await response.text();
    if (response.status >= 500) message = 'The server could not complete this request. Try again.';
    else if (response.status === 401) message = 'Authentication required or credentials invalid.';
    else if (response.status === 403) message = 'You do not have permission to perform this action.';
    else if (message.length > 300 || message.trim().startsWith('<')) message = '';
    throw new ApiError(response.status, message || 'Request failed (' + response.status + ').');
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}
export function json(method: string, body: unknown): RequestInit {
  return { method, headers: { 'Content-Type': 'application/json' }, body: JSON.stringify(body) };
}
export function componentPath(namespace: string, name: string) {
  return '/api/components/' + encodeURIComponent(namespace) + '/' + encodeURIComponent(name);
}
export function listPath(filters: Filters, offset: number, limit: number) {
  const query = new URLSearchParams({ q: filters.q, offset: String(offset), limit: String(limit) });
  if (filters.namespace) query.set('namespace', filters.namespace);
  return '/api/components?' + query.toString();
}
export function canWrite(user: User) { return ['admin', 'member'].includes(user.role.toLowerCase()); }
export function canManageUsers(user: User) { return user.role.toLowerCase() === 'admin'; }
export const userRoles = ['viewer', 'member', 'admin'] as const;
export function createUserRequest(username: string, password: string, role: string): RequestInit {
  if (!/^[a-z0-9][a-z0-9_.-]{0,62}$/.test(username)) throw new Error('Use 1–63 lowercase letters, digits, dots, underscores or hyphens; start with a letter or digit.');
  const bytes = new TextEncoder().encode(password).length;
  if (bytes < 12 || bytes > 1024) throw new Error('Password must contain 12–1024 UTF-8 bytes.');
  if (!userRoles.some(value => value === role)) throw new Error('Select a valid role.');
  return json('POST', { username, password, role });
}
// Never display arbitrary server error bodies for credential submissions.
export function createUserError(error: unknown) {
  if (error instanceof ApiError) {
    if (error.status === 409) return 'That username is already registered.';
    if (error.status === 400 || error.status === 422) return 'The server rejected the account details. Check the username, password and role.';
    if (error.status === 401 || error.status === 403) return 'Administrator access is required.';
  }
  return 'Could not create the user. Try again.';
}
export function errorMessage(error: unknown) { return error instanceof Error ? error.message : 'Request failed. Try again.'; }
export function importBody(format: 'json' | 'yaml', value: string) {
  if (new TextEncoder().encode(value).length > 65536) throw new Error('Descriptor exceeds the 64 KiB size limit.');
  return format === 'json'
    ? { path: '/api/catalog/import', init: json('POST', JSON.parse(value)) }
    : { path: '/api/catalog/import/yaml', init: { method: 'POST', headers: { 'Content-Type': 'application/yaml' }, body: value } };
}
