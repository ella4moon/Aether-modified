import { Switch } from "@/components/ui/switch"
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select"
import { useConnectionStore } from "@/state/connectionStore"
import type { FinalProxyProfile } from "@/types/connection"

const INPUT =
  "h-8 min-w-0 w-full rounded-md bg-black/20 px-2 text-xs text-foreground ring-1 ring-white/10 outline-none focus:ring-primary disabled:opacity-50"

export function FinalProxySettings() {
  const proxy = useConnectionStore((s) => s.profile.final_proxy)
  const status = useConnectionStore((s) => s.status)
  const update = useConnectionStore((s) => s.setFinalProxy)
  const locked = status.state !== "Idle" && status.state !== "Error"
  return (
    <div className="flex flex-col gap-2 rounded-md bg-black/10 p-2 ring-1 ring-white/10">
      <div className="flex items-center justify-between gap-3">
        <span className="text-xs text-muted-foreground">Use a final proxy</span>
        <Switch
          checked={proxy.enabled}
          disabled={locked}
          onCheckedChange={(enabled) => update({ enabled })}
          aria-label="Use a final proxy"
        />
      </div>
      {proxy.enabled && (
        <>
          <p className="text-xs leading-5 text-muted-foreground">
            Websites receive connections from this proxy, reached through
            Aether.
          </p>
          <Select
            value={proxy.kind}
            disabled={locked}
            onValueChange={(kind: FinalProxyProfile["kind"]) =>
              update({ kind })
            }
          >
            <SelectTrigger
              className="w-full text-xs"
              aria-label="Final proxy type"
            >
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="socks5">SOCKS5</SelectItem>
              <SelectItem value="http">HTTP CONNECT</SelectItem>
            </SelectContent>
          </Select>
          <div className="grid grid-cols-[1fr_5rem] gap-2">
            <label className="min-w-0 text-xs text-muted-foreground">
              Host
              <input
                className={INPUT}
                value={proxy.host}
                disabled={locked}
                onChange={(e) => update({ host: e.target.value })}
                placeholder="proxy.example.com"
                aria-label="Final proxy host"
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
              />
            </label>
            <label className="text-xs text-muted-foreground">
              Port
              <input
                className={INPUT}
                value={proxy.port}
                disabled={locked}
                onChange={(e) =>
                  update({
                    port: e.target.value.replace(/\D/g, "").slice(0, 5),
                  })
                }
                inputMode="numeric"
                placeholder="1080"
                aria-label="Final proxy port"
              />
            </label>
          </div>
          <div className="flex items-center justify-between gap-3">
            <span className="text-xs text-muted-foreground">
              Username and password
            </span>
            <Switch
              checked={proxy.authenticate}
              disabled={locked}
              onCheckedChange={(authenticate) => update({ authenticate })}
              aria-label="Final proxy authentication"
            />
          </div>
          {proxy.authenticate && (
            <div className="flex flex-col gap-2">
              <input
                className={INPUT}
                value={proxy.username}
                disabled={locked}
                onChange={(e) => update({ username: e.target.value })}
                placeholder="Username"
                aria-label="Final proxy username"
                autoComplete="off"
                autoCapitalize="none"
                spellCheck={false}
              />
              <input
                className={INPUT}
                type="password"
                value={proxy.password}
                disabled={locked}
                onChange={(e) => update({ password: e.target.value })}
                placeholder="Password"
                aria-label="Final proxy password"
                autoComplete="off"
              />
              <p className="text-[11px] leading-4 text-muted-foreground">
                Credentials stay in memory. Enter them again after restarting
                the app.
              </p>
            </div>
          )}
          <p className="text-[11px] leading-4 text-muted-foreground">
            TCP only. Connections fail if the final proxy fails. UDP is
            disabled. Aether routing rules and the organization Gateway are
            inactive in this mode.
          </p>
        </>
      )}
    </div>
  )
}
