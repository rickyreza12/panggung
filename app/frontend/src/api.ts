export interface User { id: string | number; username: string; role: string }
export interface Component { name: string; namespace: string; owner: string; description: string; component_type: string | null; lifecycle: string | null; descriptor: unknown }
export interface Relation { type: string; targetRef: string }
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
export function errorMessage(error: unknown) { return error instanceof Error ? error.message : 'Request failed. Try again.'; }
export function importBody(format: 'json' | 'yaml', value: string) {
  if (new TextEncoder().encode(value).length > 65536) throw new Error('Descriptor exceeds the 64 KiB size limit.');
  return format === 'json'
    ? { path: '/api/catalog/import', init: json('POST', JSON.parse(value)) }
    : { path: '/api/catalog/import/yaml', init: { method: 'POST', headers: { 'Content-Type': 'application/yaml' }, body: value } };
}
