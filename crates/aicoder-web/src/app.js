"use strict";
const $ = id => document.getElementById(id);
let token = "", current = null, nextId = 1, question = null, polling = null, streaming = null;
const LIMIT = 256 * 1024;
const noticeScopes = new Map();
let command = null, uiQuestion = null, uiTarget = null, uiQuestionKey = "", lastCommandResult = "", observedRun = null;
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
async function refresh() {
  const state = await api("/api/state"); nextId = state.next_prompt_id; show(state.run); command = state.command;
  const busyCommand = Boolean(command && !command.result), running = Boolean(state.run && !state.run.result);
  $("submit").disabled = !token || running || busyCommand; $("extension-run").disabled = !token || running || busyCommand;
  $("cancel").disabled = !(running || busyCommand);
  $("cancel").textContent = busyCommand ? "Cancel command" : "Cancel run";
  if (busyCommand) { $("status").textContent = "Running command"; $("detail").textContent = "Command is awaiting completion"; }
  if (command?.result && lastCommandResult !== command.id) { lastCommandResult = command.id; if (command.result.notice) append("\n" + command.result.notice + "\n"); else if (command.result.message) append("\n[" + command.result.message + "]\n"); }
  const commandUi = command && (!state.run || command.request_id > state.run.request_id);
  const ui = commandUi ? command.ui : state.run?.ui;
  uiTarget = commandUi ? `/api/commands/${command.id}/ui` : state.run ? `/api/runs/${state.run.run_id}/ui` : null;
  const liveScopes = new Set([state.command?.ui?.scope, state.run?.ui?.scope]);
  for (const scope of noticeScopes.keys()) if (!liveScopes.has(scope)) noticeScopes.delete(scope);
  showNotices(state.command?.ui); showNotices(state.run?.ui);
  showUi(ui);
  if (command?.result?.status === "submitted" && state.run && command.result.run_id === state.run.run_id && observedRun !== state.run.run_id) observe(state.run.run_id, `/api/runs/${state.run.run_id}/events`);
  return state;
}
function showNotices(ui) {
  if (!ui) return;
  const seen = noticeScopes.get(ui.scope) || {sequence:0, omitted:0};
  if (ui.omitted_notices > seen.omitted) append("\n[Earlier extension notices are no longer retained.]\n");
  for (const notice of ui.notices) if (notice.sequence > seen.sequence) { append(`\n[${notice.owner} · ${notice.level}] ${notice.text}\n`); seen.sequence = notice.sequence; }
  seen.omitted = ui.omitted_notices; noticeScopes.set(ui.scope, seen);
}
function showUi(ui) {
  $("extension-status").textContent = (ui?.statuses || []).map(status => status.owner + ": " + status.text).join(" · ");
  const next = ui?.questions?.[0] || null;
  const key = next ? `${next.id.scope}/${next.id.sequence}` : "";
  if (key !== uiQuestionKey) {
    uiQuestionKey = key; uiQuestion = next;
    $("extension-options").replaceChildren(); $("extension-input").value = "";
    if (next) {
      $("extension-title").textContent = next.prompt.title; $("extension-owner").textContent = next.owner;
      const select = next.prompt.kind === "select"; $("extension-options").hidden = !select; $("extension-input").hidden = select;
      if (select) for (const item of next.prompt.options) { const option = document.createElement("option"); option.value = item.id; option.textContent = item.label; $("extension-options").append(option); }
      else { $("extension-input").value = next.prompt.initial; $("extension-input").maxLength = next.prompt.max_bytes; }
    }
  }
  $("extension-question").hidden = Boolean(question) || !next;
  inputBudget();
}
function inputBudget() {
  const input = uiQuestion?.prompt.kind === "input";
  $("extension-input-budget").hidden = !input;
  $("extension-answer").disabled = false;
  if (input) {
    const text = $("extension-input").value, bytes = new TextEncoder().encode(text).length;
    const valid = bytes <= uiQuestion.prompt.max_bytes && (uiQuestion.prompt.multiline || !/[\r\n]/.test(text));
    $("extension-input-budget").textContent = `${bytes} / ${uiQuestion.prompt.max_bytes} UTF-8 bytes${uiQuestion.prompt.multiline ? "" : " · single line"}`;
    $("extension-answer").disabled = !valid;
  }
}
$("extension-input").addEventListener("input", inputBudget);
async function observe(id, path) {
  observedRun = id;
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
  try { const catalog = await api("/api/commands"); $("extension-command").replaceChildren(); for (const spec of catalog.commands) {const option=document.createElement("option");option.value=spec.name;option.textContent=spec.name + " · " + spec.description;$("extension-command").append(option);} $("extension-commands").hidden = !catalog.commands.length; const state = await refresh(); $("connect-panel").hidden = true; $("output").textContent = ""; if (state.run && !state.run.result) append("[Attached through status; prior live events may be unavailable]\n"); clearInterval(polling); polling = setInterval(()=>refresh().catch(fail),1000); }
  catch (error) { token = ""; show(null); fail(error); }
});
$("submit").addEventListener("click", async () => {
  $("error").textContent = ""; $("submit").disabled = true;
  try { await refresh(); const text = $("prompt").value; const result = await api("/api/runs",{id:nextId,text}); $("prompt").value = ""; append("\nYOU\n" + text + "\n\n"); await refresh(); observe(result.run_id,result.events); }
  catch (error) { fail(error); await refresh().catch(fail); }
});
$("revoke").addEventListener("click",()=>api("/api/permissions/revoke",{}).then(refresh).catch(fail));
$("cancel").addEventListener("click",()=>api(command && !command.result ? `/api/commands/${command.id}/cancel` : `/api/runs/${current}/cancel`,{}).then(refresh).catch(fail));
$("extension-run").addEventListener("click", async()=>{
  try { const args = JSON.parse($("extension-arguments").value); await refresh(); await api("/api/commands",{id:nextId,name:$("extension-command").value,arguments:args}); await refresh(); } catch(error) {fail(error);await refresh().catch(fail);}
});
async function answerExtension(dismiss) {
  const pending=uiQuestion, target=uiTarget; if (!pending || !target) return;
  const answer=dismiss ? {kind:"dismissed"} : pending.prompt.kind === "select" ? {kind:"choice",id:$("extension-options").value} : {kind:"text",text:$("extension-input").value};
  try { await api(target,{question_id:pending.id,answer}); await refresh(); } catch(error) {fail(error);await refresh().catch(fail);}
}
$("extension-answer").addEventListener("click",()=>answerExtension(false));
$("extension-dismiss").addEventListener("click",()=>answerExtension(true));
for (const [id,decision] of [["deny","deny"],["once","allow_once"],["session","allow_session"]]) $(id).addEventListener("click", async()=>{
  const pending=question, run=current; if (!pending) return;
  try { await api(`/api/runs/${run}/approval`,{approval_id:pending.id,decision}); await refresh(); } catch(error) {fail(error);await refresh().catch(fail);}
});
