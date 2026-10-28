// soi-agent-integration: written by soi (Settings ▸ Agents).
// Tells soi what Pi is doing, for its agents panel, and which session it is,
// so soi can link it to the tab's task. Outside soi it does nothing.
// soi only replaces this file while this first line is here.
import { connect } from "node:net"

export default function (pi) {
  const socket = process.env.SOI_SOCKET
  const tab = process.env.SOI_TAB_ID
  if (!socket || !tab) return

  // Ephemeral sessions (--no-session) can't be resumed, so they aren't linked.
  let session = ""
  const notice = (ctx) => {
    const manager = ctx?.sessionManager
    if (manager?.getSessionFile?.()) session = manager.getSessionId?.() ?? ""
  }

  // The model and how full its context is, as Pi's footer shows them.
  let usage
  const measure = (ctx) => {
    const context = ctx?.getContextUsage?.()
    const model = ctx?.model
    if (!model || !context?.tokens) return
    usage = { model: model.name || model.id, context: Math.round(context.tokens) }
    if (context.contextWindow > 0) usage.window = Math.round(context.contextWindow)
  }

  let last = ""
  const send = (state, message = "") => {
    const report = { tab, state, message }
    if (session) report.session = { provider: "pi", id: session }
    if (usage) report.usage = usage
    const line = JSON.stringify(report) + "\n"
    if (line === last) return
    try {
      // Only a delivered report counts: one soi missed is sent again.
      const client = connect(socket, () => {
        last = line
        client.end(line)
      })
      client.on("error", () => {})
    } catch {}
  }

  // A run lasts from agent_start until agent_settled: retries, compaction and
  // queued messages can start more after agent_end.
  let running = false
  let asking = 0
  let error = ""
  const settle = () => {
    if (asking > 0) return
    if (running) send("working")
    else if (error) send("input", error)
    else send("done")
  }

  pi.on("session_start", async (_event, ctx) => {
    notice(ctx)
  })
  pi.on("agent_start", async (_event, ctx) => {
    notice(ctx)
    running = true
    error = ""
    settle()
  })
  pi.on("tool_execution_start", async () => {
    settle()
  })
  pi.on("turn_end", async (_event, ctx) => {
    measure(ctx)
    settle()
  })
  pi.on("agent_end", async (event) => {
    const reply = [...(event.messages ?? [])].reverse().find((m) => m.role === "assistant")
    error = reply?.stopReason === "error" ? reply.errorMessage || "Error" : ""
  })
  pi.on("agent_settled", async (event, ctx) => {
    notice(ctx)
    measure(ctx)
    running = false
    if (event.aborted) error = ""
    settle()
  })
  pi.on("ui_prompt_start", async (event) => {
    asking++
    send("input", event.title || "Waiting for your answer")
  })
  pi.on("ui_prompt_end", async () => {
    asking = Math.max(0, asking - 1)
    settle()
  })
}
