import { useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { errorMessage, json, request } from './api';
import type { User } from './api';

interface Team { id: string | number; name: string; members: User[] }

export default function Teams({ run, admin }: { run: typeof request; admin: boolean }) {
  const [teams, setTeams] = useState<Team[]>([]);
  const [users, setUsers] = useState<User[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState('');
  const [usersError, setUsersError] = useState('');
  const [notice, setNotice] = useState('');
  const [busy, setBusy] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [name, setName] = useState('');
  const [selected, setSelected] = useState<Record<string, string>>({});
  useEffect(() => {
    const controller = new AbortController();
    setLoading(true); setError(''); setUsersError('');
    run<Team[]>('/api/organization/teams', {signal: controller.signal})
      .then(result => { if (!controller.signal.aborted) setTeams(result); })
      .catch(e => { if (!controller.signal.aborted) setError(errorMessage(e)); })
      .finally(() => { if (!controller.signal.aborted) setLoading(false); });
    if (admin) run<User[]>('/api/organization/users', {signal: controller.signal})
      .then(result => { if (!controller.signal.aborted) setUsers(result); })
      .catch(e => { if (!controller.signal.aborted) setUsersError(errorMessage(e)); });
    return () => controller.abort();
  }, [run, admin, refresh]);
  async function create(event: FormEvent) {
    event.preventDefault(); setBusy(true); setError(''); setNotice('');
    try { await run<Team>('/api/organization/teams', json('POST', {name: name.trim()})); setName(''); setNotice('Team created.'); setRefresh(n=>n+1); }
    catch(e) { setError(errorMessage(e)); } finally { setBusy(false); }
  }
  async function membership(team: Team, id: string, method: 'PUT' | 'DELETE') {
    setBusy(true); setError(''); setNotice('');
    try {
      await run<void>('/api/organization/teams/'+encodeURIComponent(team.id)+'/members/'+encodeURIComponent(id), {method});
      setSelected(old=>({...old,[team.id]:''})); setNotice(method==='PUT'?'Member added.':'Member removed.'); setRefresh(n=>n+1);
    } catch(e) { setError(errorMessage(e)); } finally { setBusy(false); }
  }
  return <>
    <div className="heading"><h1 tabIndex={-1} ref={node=>{node?.focus();}}>Teams</h1><p>Browse engineering teams and their registered members.</p></div>
    {error&&<p className="error" role="alert">{error}</p>}{notice&&<p className="notice" role="status">{notice}</p>}
    {admin&&<form className="panel filters" onSubmit={create}><label>New team name<input required maxLength={100} value={name} onChange={e=>setName(e.target.value)}/></label><button className="primary" disabled={busy||!name.trim()}>Create team</button></form>}
    {loading?<p role="status">Loading teams…</p>:error&&teams.length===0?<button onClick={()=>setRefresh(n=>n+1)}>Try again</button>:teams.length===0?<div className="panel empty"><h2>No teams registered</h2><p>{admin?'Create a team to get started.':'Ask an administrator to create a team.'}</p></div>:teams.map(team=><section className="panel section" key={team.id}><h2>{team.name}</h2>
      {team.members.length===0?<p>No members assigned.</p>:<ul className="relations">{team.members.map(member=><li key={member.id}><strong>{member.username}</strong><span>{member.role}</span>{admin&&<button className="danger" disabled={busy} aria-label={'Remove '+member.username+' from '+team.name} onClick={()=>{if(window.confirm('Remove '+member.username+' from '+team.name+'?'))void membership(team,String(member.id),'DELETE');}}>Remove</button>}</li>)}</ul>}
      {admin&&(usersError?<><p role="alert" className="error">{usersError}</p><button onClick={()=>setRefresh(n=>n+1)}>Retry users</button></>:<form className="filters" onSubmit={e=>{e.preventDefault();const id=selected[team.id];if(id)void membership(team,id,'PUT');}}><label>Add member to {team.name}<select value={selected[team.id]??''} onChange={e=>setSelected(old=>({...old,[team.id]:e.target.value}))}><option value="">Select a registered user</option>{users.filter(u=>!team.members.some(m=>m.id===u.id)).map(u=><option value={String(u.id)} key={u.id}>{u.username}</option>)}</select></label><button disabled={busy||!selected[team.id]}>Add member</button></form>)}
    </section>)}
  </>;
}
