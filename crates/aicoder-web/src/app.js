"use strict";
const $ = id => document.getElementById(id);
let token = "", current = null, nextId = 1, question = null, polling = null, streaming = null;
const LIMIT = 256 * 1024;
function append(text) { const out = $("output"); const value = out.textContent + text; out.textContent = value.length > LIMIT ? "[Earlier preview omitted]\n" + value.slice(-LIMIT) : value; out.scrollTop = out.scrollHeight; }
function fail(error) { $("error").textContent = error.message || String(error); }
async function api(path, body) {
  const response = await fetch(path, { method: body === undefined ? "GET" : "POST", headers: { Authorization: "Bearer " + token, ...(body === undefined ? {} : {"Content-Type":"application/json"}) }, body: body === undefined ? undefined : JSON.stringify(body), cache: "no-store" });
  const value = await response.json();
  if (!response.ok) throw new Error(value.error?.message || "HTTP " + response.status);
  return value;
}
function show(run) {
  current = run?.run_id || null;
  const running = Boolean(run && !run.result);
  $("status").textContent = run?.result?.status || (running ? "Running" : "Ready");
  $("cancel").disabled = !running;
  $("revoke").disabled = !token;
  $("submit").disabled = !token || running;
  $("prompt").disabled = !token;
  $("detail").textContent = run?.result ? `Rounds ${run.result.rounds ?? "?"} · Tokens ${run.result.usage?.total_tokens ?? "unknown"}` : running ? "Host owns the active run" : "Ready for the next task";
  question = run?.approvals?.[0] || null;
  $("approval").hidden = !question;
  if (question) {
    const preview = JSON.stringify({tool: question.invocation.name, session_scope: question.session, session_choice: "All arguments for this tool in the displayed session/workspace until revoked or host shutdown", preview: question.invocation.preview, arguments: question.invocation.arguments}, null, 2);
    $("preview").textContent = preview.length > 65536 ? preview.slice(0, 65536) + "\n[Browser preview clipped; inspect full pending request through authenticated status.]" : preview;
    $("session").disabled = !question.session;
  }
}
async function refresh() { const state = await api("/api/state"); nextId = state.next_prompt_id; show(state.run); return state; }
async function observe(id, path) {
  if (streaming) streaming.abort();
  const controller = new AbortController(); streaming = controller;
  try {
    const response = await fetch(path, {headers:{Authorization:"Bearer " + token}, signal:controller.signal, cache:"no-store"});
    if (!response.ok) throw new Error((await response.json()).error?.message || "Event stream unavailable");
    const reader = response.body.getReader(), decoder = new TextDecoder("utf-8", {fatal:true});
    let pending = "";
    while (true) {
      const {done, value} = await reader.read(); if (done) break;
      pending += decoder.decode(value, {stream:true});
      if (pending.length > 8 * 1024 * 1024) throw new Error("Event exceeds browser preview limit");
      let end;
      while ((end = pending.indexOf("\n\n")) !== -1) {
        const block = pending.slice(0,end); pending = pending.slice(end+2);
        const lines = block.split("\n"), kind = lines.find(l=>l.startsWith("event:"))?.slice(6).trim();
        const data = lines.filter(l=>l.startsWith("data:")).map(l=>l.slice(5).trimStart()).join("\n");
        if (!data) continue;
        const frame = JSON.parse(data);
        if (kind === "agent") {
          if (frame.run_id !== id) throw new Error("Unexpected run identity");
          const event = frame.event;
          if (event.type === "content_chunk") append(event.delta);
          if (event.type === "run_approval_requested") await refresh();
        } else if (kind === "observation_error") { fail(new Error("Live events are incomplete; status and approvals remain available.")); }
        else if (kind === "result") { append(`\n\n[${frame.status}]\n`); await refresh(); }
      }
    }
    decoder.decode();
    if (pending.trim()) throw new Error("Incomplete event frame; inspect run status");
  } catch (error) { if (error.name !== "AbortError") fail(error); }
  finally { if (streaming === controller) streaming = null; await refresh().catch(fail); }
}
$("connect").addEventListener("click", async () => {
  token = $("token").value; $("token").value = ""; $("error").textContent = "";
  try { const state = await refresh(); $("connect-panel").hidden = true; $("output").textContent = ""; if (state.run && !state.run.result) append("[Attached through status; prior live events may be unavailable]\n"); clearInterval(polling); polling = setInterval(()=>refresh().catch(fail),1000); }
  catch (error) { token = ""; show(null); fail(error); }
});
$("submit").addEventListener("click", async () => {
  $("error").textContent = ""; $("submit").disabled = true;
  try { await refresh(); const text = $("prompt").value; const result = await api("/api/runs",{id:nextId,text}); $("prompt").value = ""; append("\nYOU\n" + text + "\n\n"); await refresh(); observe(result.run_id,result.events); }
  catch (error) { fail(error); await refresh().catch(fail); }
});
$("revoke").addEventListener("click",()=>api("/api/permissions/revoke",{}).then(refresh).catch(fail));
$("cancel").addEventListener("click",()=>api(`/api/runs/${current}/cancel`,{}).then(refresh).catch(fail));
for (const [id,decision] of [["deny","deny"],["once","allow_once"],["session","allow_session"]]) $(id).addEventListener("click", async()=>{
  const pending=question, run=current; if (!pending) return;
  try { await api(`/api/runs/${run}/approval`,{approval_id:pending.id,decision}); await refresh(); } catch(error) {fail(error);await refresh().catch(fail);}
});
