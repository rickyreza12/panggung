import { useState } from 'react';
import type { FormEvent } from 'react';
import { createUserError, createUserRequest, request, userRoles } from './api';
import type { User } from './api';
import './account.css';

export default function CreateUser({ run, onCreated }: { run: typeof request; onCreated: (user: User) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  async function submit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (busy) return;
    const form = event.currentTarget;
    const data = new FormData(form);
    setError('');
    let init: RequestInit;
    try {
      init = createUserRequest(String(data.get('username')), String(data.get('password')), String(data.get('role')));
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Check the account details.');
      return;
    }
    setBusy(true);
    try {
      const user = await run<User>('/api/organization/users', init);
      form.reset();
      onCreated(user);
    } catch (e) {
      setError(createUserError(e));
    } finally {
      setBusy(false);
    }
  }
  return <section className="panel section" aria-labelledby="create-user-heading">
    <h2 id="create-user-heading">Create user</h2>
    <p className="muted">Register a local account. Team membership is assigned separately.</p>
    <form className="form" onSubmit={submit} aria-busy={busy}>
      <fieldset disabled={busy} className="account-fields">
        <label>Username<input name="username" autoComplete="off" autoCapitalize="none" spellCheck={false} maxLength={63} required aria-describedby="new-user-name-help" /></label>
        <small id="new-user-name-help">Lowercase letters, digits, dots, underscores and hyphens. Start with a letter or digit.</small>
        <label>Password<input name="password" type="password" autoComplete="new-password" maxLength={1024} required aria-describedby="new-user-password-help" /></label>
        <small id="new-user-password-help">12–1024 UTF-8 bytes. Use a strong, unique password.</small>
        <label>Role<select name="role" defaultValue="viewer">{userRoles.map(role => <option value={role} key={role}>{role === 'admin' ? 'Administrator' : role === 'member' ? 'Member' : 'Viewer'}</option>)}</select></label>
        <small>Viewers read catalog records; members can edit the catalog; administrators manage users and teams.</small>
      </fieldset>
      {error && <p className="error" role="alert">{error}</p>}
      <div className="actions"><button className="primary" disabled={busy}>{busy ? 'Creating user…' : 'Create user'}</button></div>
    </form>
  </section>;
}
