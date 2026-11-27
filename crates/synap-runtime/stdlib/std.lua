-- synap 标准库 v1。它本身是一个普通数据插件：随包发行，走同一套接口。
-- 词汇表要小、稳、慢；领域能力先在插件自己的命名空间里长。

plugin "std" { api = 1 }

trait "std/Titled@1" {
  read = { title = "text" },
  write = { set_title = { args = { title = "text" } } },
  doc = "有标题",
}

trait "std/HasText@1" {
  read = { body = "text" },
  write = { set_body = { args = { body = "text" } } },
  doc = "有正文",
}

trait "std/Checkable@1" {
  read = { done = "bool" },
  write = { set_done = { args = { done = "bool" } } },
  doc = "可完成的事项",
}

trait "std/Temporal@1" {
  read = { at = "time?", due = "time?" },
  write = { set_time = { args = { at = "time?", due = "time?" } } },
  doc = "有时间点或期限",
}

trait "std/Tagged@1" {
  read = { tags = "[string]" },
  write = { set_tags = { args = { tags = "[string]" } } },
  doc = "带标签",
}

trait "std/Tree@1" {
  read = { children = "[ref]" },
  write = { set_children = { args = { children = "[ref]" } } },
  doc = "有子节点",
}

trait "std/Context@1" {
  read = { members = "[ref]" },
  write = {
    set_members = { args = { members = "[ref]" } },
    add_member = { args = { member = "ref" } },
    remove_member = { args = { member = "ref" } },
  },
  doc = "情景：一组实体的成员关系",
}

trait "std/Attention@1" {
  read = { reason = "text", since = "hlc", due = "time?" },
  write = {
    set_reason = { args = { reason = "text" } },
    set_due = { args = { due = "time?" } },
  },
  doc = "在等用户处理的东西",
}

component "std/Title@1" {
  fields = { title = "text" },
  merge = "lww",
  impl = { "std/Titled@1" },
  construct = function(args)
    return { title = args.title or "" }
  end,
}

component "std/Text@1" {
  fields = { body = "text" },
  merge = "text-3way",
  impl = { "std/HasText@1" },
  construct = function(args)
    return { body = args.text or args.body or "" }
  end,
}

component "std/Check@1" {
  fields = { done = "bool", done_at = "hlc?" },
  merge = "lww",
  impl = { "std/Checkable@1" },
  construct = function(args)
    return { done = args.done == true and true or false, done_at = args.done_at }
  end,
}

component "std/Time@1" {
  fields = { at = "time?", due = "time?" },
  merge = "lww",
  impl = { "std/Temporal@1" },
  construct = function(args)
    return { at = args.at, due = args.due }
  end,
}

component "std/Tags@1" {
  fields = { tags = "[string]" },
  merge = "set",
  impl = { "std/Tagged@1" },
  construct = function(args)
    return { tags = args.tags or {} }
  end,
}

component "std/Children@1" {
  fields = { children = "[ref]" },
  merge = "seq",
  impl = { "std/Tree@1" },
  construct = function(args)
    return { children = args.children or {} }
  end,
}

component "std/Members@1" {
  fields = { members = "[ref]" },
  merge = "set",
  impl = { "std/Context@1" },
  construct = function(args)
    return { members = args.members or {} }
  end,
}

component "std/Attention@1" {
  fields = { reason = "text", since = "hlc", due = "time?" },
  merge = "lww",
  impl = { "std/Attention@1" },
  construct = function(args)
    return { reason = args.reason or "", since = args.since, due = args.due }
  end,
}

schema "std/context" {
  compose = {
    title = "std/Title@1",
    body = "std/Text@1",
    time = "std/Time@1?",
    members = "std/Members@1",
  },
  doc = "情景：标题 + 正文 + 可选时间 + 成员",
}

-- 成员增删需要去重，显式实现覆盖组件委托。
impl("std/Context@1", "std/context@1", {
  extract = function(r)
    return { members = r.members }
  end,
  set_members = function(r, a)
    r.members = a.members
    return r
  end,
  add_member = function(r, a)
    local members = r.members or {}
    for i = 1, #members do
      if members[i].id == a.member.id then
        return r
      end
    end
    table.insert(members, a.member)
    r.members = members
    return r
  end,
  remove_member = function(r, a)
    local members = r.members or {}
    local kept = {}
    for i = 1, #members do
      if members[i].id ~= a.member.id then
        table.insert(kept, members[i])
      end
    end
    r.members = kept
    return r
  end,
})
