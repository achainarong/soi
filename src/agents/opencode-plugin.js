// soi-agent-integration: written by soi (Settings ▸ Agents).
// Tells soi what OpenCode is doing, for its agents panel, and which session
// it is, so soi can link it to the tab's task. Outside soi it does nothing.
// soi only replaces this file while this first line is here.
import { connect } from "node:net"

// What to do with each event, or nothing outside soi. `describe` looks up a
// model's name and context window.
const reporter = (describe) => {
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

  // The main session's model and its latest reply's tokens.
  let model
  let usage
  // Names and context windows, by provider and model; the model's ID until
  // OpenCode answers.
  const models = new Map()
  const choose = (providerID, id) => {
    if (!id) return
    const key = `${providerID}/${id}`
    model = key
    if (models.has(key)) return
    models.set(key, { name: id })
    Promise.resolve()
      .then(() => describe(providerID, id))
      .then((info) => {
        if (info?.name) models.set(key, info)
      })
      .catch(() => {})
  }
  // The state last reported, to report it again with new usage.
  let current

  let last = ""
  const send = (state, message = "") => {
    current = { state, message }
    const report = { tab, state, message }
    if (main) report.session = { provider: "opencode", id: main }
    if (usage) report.usage = usage
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
  // What the main session said last: what it did.
  let said = ""
  const settle = () => {
    if (asking > 0) return
    if (busy.size > 0) send("working")
    else send("done", said)
  }

  const asked = (message) => {
    asking++
    send("input", message)
  }
  const answered = () => {
    asking = Math.max(0, asking - 1)
    settle()
  }

  // Subagents' sessions have their own context.
  const isMain = (id) => id && !children.has(id) && (!main || id === main)
  // 1.x: the user's messages have text parts too.
  const replies = new Set()
  const spoke = (id, text) => {
    if (isMain(id) && text?.trim()) said = text.trim().slice(0, 1000)
  }
  const used = (id, tokens) => {
    if (!isMain(id) || !tokens) return
    const context =
      (tokens.input ?? 0) +
      (tokens.output ?? 0) +
      (tokens.reasoning ?? 0) +
      (tokens.cache?.read ?? 0) +
      (tokens.cache?.write ?? 0)
    // Replies start out with none yet.
    if (context === 0 || !model) return
    const info = models.get(model)
    usage = { model: info.name, context: Math.round(context) }
    if (info.window > 0) usage.window = Math.round(info.window)
    if (current) send(current.state, current.message)
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
        if (busy.size === 0) said = ""
        busy.add(p.sessionID)
        settle()
        break
      case "session.execution.succeeded":
      case "session.execution.interrupted":
        busy.delete(p.sessionID)
        settle()
        break
      case "session.step.started":
        if (isMain(p.sessionID)) choose(p.model?.providerID, p.model?.id)
        break
      case "session.step.ended":
        used(p.sessionID, p.tokens)
        break
      case "session.text.ended":
        spoke(p.sessionID, p.text)
        break
      // 1.x: the reply's text, as it grows.
      case "message.part.updated":
        if (p.part?.type === "text" && replies.has(p.part.messageID))
          spoke(p.part.sessionID, p.part.text)
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
        if (status === "busy" || status === "retry") {
          if (busy.size === 0) said = ""
          busy.add(p.sessionID)
        }
        else if (status === "idle") busy.delete(p.sessionID)
        settle()
        break
      }
      case "message.updated": {
        const info = p.info
        if (info?.role !== "assistant" || !isMain(info.sessionID)) break
        replies.add(info.id)
        choose(info.providerID, info.modelID)
        used(info.sessionID, info.tokens)
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
const start = (describe) => {
  if (started) return undefined
  const report = reporter(describe)
  started = report !== undefined
  return report
}

// A model as OpenCode describes it.
const info = (model, id) =>
  model && { name: model.name || id, window: model.limit?.context }

// OpenCode 1.x
const server = async ({ client } = {}) => {
  const report = start(async (providerID, id) => {
    const result = await client?.config?.providers?.()
    const provider = (result?.data ?? result)?.providers?.find((p) => p.id === providerID)
    return info(provider?.models?.[id], id)
  })
  if (!report) return {}
  return { event: async ({ event }) => report(event.type, event.properties ?? {}) }
}

// OpenCode 2.x
const setup = (opencode) => {
  const report = start(async (providerID, id) => {
    const result = await opencode.model?.list?.()
    const list = Array.isArray(result) ? result : (result?.data ?? [])
    const same = (m) => m.id === id || m.modelID === id
    return info(list.find((m) => m.providerID === providerID && same(m)) ?? list.find(same), id)
  })
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
