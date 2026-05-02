import { Component, For, createSignal, onMount } from 'solid-js';
import { usageApi } from '../api/client';

interface DashboardProps { data: any; }

const Dashboard: Component<DashboardProps> = (props) => {
  const [usage, setUsage] = createSignal<any>(null);
  onMount(async () => { try { const d = await usageApi.get(); setUsage(d); } catch (e) {} });

  return (
    <div class="dashboard">
      <header class="page-header"><h1>Dashboard</h1><p>Overview of your OpenProxy configuration</p></header>
      <div class="grid grid-4 mb-4">
        <div class="stat-card"><div class="stat-value">{props.data.providersCount || 0}</div><div class="stat-label">Providers</div></div>
        <div class="stat-card"><div class="stat-value">{props.data.combosCount || 0}</div><div class="stat-label">Combos</div></div>
        <div class="stat-card"><div class="stat-value">{props.data.keysCount || 0}</div><div class="stat-label">API Keys</div></div>
        <div class="stat-card"><div class="stat-value">{props.data.proxyPoolsCount || 0}</div><div class="stat-label">Proxy Pools</div></div>
      </div>
      <div class="card">
        <div class="card-header"><h3 class="card-title">Recent Activity</h3></div>
        {props.data.recentActivity?.length > 0 ? (
          <table class="table">
            <thead><tr><th>Type</th><th>Name</th><th>Status</th><th>Timestamp</th></tr></thead>
            <tbody>
              <For each={props.data.recentActivity}>{(item) => (
                <tr><td><span class="badge badge-success">{item.type}</span></td><td>{item.name}</td><td><span class={`badge ${item.status === 'active' ? 'badge-success' : 'badge-warning'}`}>{item.status}</span></td><td class="text-muted">{item.timestamp}</td></tr>
              )}</For>
            </tbody>
          </table>
        ) : <p class="text-muted p-4">No recent activity</p>}
      </div>
      {usage && (
        <div class="card mt-4">
          <div class="card-header"><h3 class="card-title">Usage Statistics</h3></div>
          <div class="grid grid-3">
            <div class="stat-card"><div class="stat-value">{usage.totalRequestsLifetime || 0}</div><div class="stat-label">Total Requests (Lifetime)</div></div>
            <div class="stat-card"><div class="stat-value">{usage.history?.length || 0}</div><div class="stat-label">Recent Requests</div></div>
            <div class="stat-card"><div class="stat-value">{Object.keys(usage.dailySummary || {}).length}</div><div class="stat-label">Days Tracked</div></div>
          </div>
        </div>
      )}
    </div>
  );
};
export default Dashboard;
