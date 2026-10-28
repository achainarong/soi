// soi-agent-integration: written by soi (Settings ▸ Agents).
// Tells soi what OpenCode is doing, for its agents panel, and which session
// it is, so soi can link it to the tab's task. Outside soi it does nothing.
// soi only replaces this file while this first line is here.
import { connect } from "node:net"

// What to do with each event, or nothing outside soi.
const reporter = () => {
  const socket = process.env.SOI_SOCKET
  const tab = process.env.SOI_TAB_ID
  if (!socket || !tab) return undefined
  // 2.x's background server works for every terminal, but has the
  // environment of the tab that started it: it can't say which tab asks.
  if (process.argv.includes("--service")) return undefined

  // The session the user works in; subagents' sessions have a parent.
  let main = ""
  const children = new Set()
  const notice = (info) => {
    if (!info?.id) return
    if (info.parentID) children.add(info.id)
    else main = info.id
  }
  const mainOf = (id) => {
    if (!main && id && !children.has(id)) main = id
  }

  let last = ""
  const send = (state, message = "") => {
    const report = { tab, state, message }
    if (main) report.session = { provider: "opencode", id: main }
    const line = JSON.stringify(report) + "\n"
    if (line === last) return
    // Set before it's delivered, so the same report right after (1.x says
    // idle twice) isn't sent again; one soi missed is.
    last = line
    const missed = () => {
      if (last === line) last = ""
    }
    try {
      const client = connect(socket, () => client.end(line))
      client.on("error", missed)
    } catch {
      missed()
    }
  }

  // Busy sessions (subagents have their own) and open questions.
  const busy = new Set()
  let asking = 0
  const settle = () => {
    if (asking > 0) return
    if (busy.size > 0) send("working")
    else send("done")
  }

  const asked = (message) => {
    asking++
    send("input", message)
  }
  const answered = () => {
    asking = Math.max(0, asking - 1)
    settle()
  }

  return (type, p) => {
    switch (type) {
      case "session.created":
      case "session.updated":
        // 1.x has the session as `info`; 2.x has its fields.
        notice(p.info ?? { id: p.sessionID, parentID: p.parentID })
        break
      // 2.x: one run of the agent.
      case "session.execution.started":
        mainOf(p.sessionID)
        busy.add(p.sessionID)
        settle()
        break
      case "session.execution.succeeded":
      case "session.execution.interrupted":
        busy.delete(p.sessionID)
        settle()
        break
      case "session.execution.failed": {
        busy.delete(p.sessionID)
        const error = p.error
        send("input", error?.data?.message ?? error?.message ?? error?.name ?? "Error")
        break
      }
      case "form.created": {
        const form = p.form ?? {}
        asked(form.title ?? form.fields?.[0]?.title ?? "Has a question")
        break
      }
      case "form.replied":
      case "form.cancelled":
        answered()
        break
      // 1.x
      case "session.status": {
        mainOf(p.sessionID)
        const status = p.status?.type
        if (status === "busy" || status === "retry") busy.add(p.sessionID)
        else if (status === "idle") busy.delete(p.sessionID)
        settle()
        break
      }
      case "session.idle":
        busy.delete(p.sessionID)
        settle()
        break
      // Both: 2.x has `message` and `action` where 1.x has `title` and
      // `permission`.
      case "permission.updated":
      case "permission.asked": {
        const action = p.permission ?? p.action
        asked(p.title ?? p.message ?? (action ? `Allow ${action}?` : "Permission needed"))
        break
      }
      case "question.asked":
        asked(p.questions?.[0]?.question ?? "Has a question")
        break
      case "permission.replied":
      case "question.replied":
      case "question.rejected":
        answered()
        break
      case "session.error": {
        const error = p.error
        send("input", error?.data?.message ?? error?.message ?? error?.name ?? "Error")
        break
      }
    }
  }
}

// Late 1.x versions call both `server` and `setup`: only the first reports.
let started = false
const start = () => {
  if (started) return undefined
  const report = reporter()
  started = report !== undefined
  return report
}

// OpenCode 1.x
const server = async () => {
  const report = start()
  if (!report) return {}
  return { event: async ({ event }) => report(event.type, event.properties ?? {}) }
}

// OpenCode 2.x
const setup = (opencode) => {
  const report = start()
  if (!report) return undefined
  const stop = new AbortController()
  ;(async () => {
    for await (const event of opencode.event.subscribe({ signal: stop.signal })) {
      report(event.type, event.data ?? {})
    }
  })().catch(() => {})
  // Unloaded, as 2.x does to reload it: the next `setup` reports.
  return () => {
    stop.abort()
    started = false
  }
}

export default { id: "soi", server, setup }
