import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Switch } from "@/components/ui/switch";
import { useConnectionStore } from "@/state/connectionStore";

interface Support {
  supported: boolean;
  elevated: boolean;
  available: boolean;
}

export function WholeLaptopSettings() {
  const enabled = useConnectionStore((s) => s.profile.whole_laptop);
  const finalProxy = useConnectionStore((s) => s.profile.final_proxy.enabled);
  const udpEnabled = useConnectionStore((s) => s.profile.final_proxy.udp_enabled);
  const status = useConnectionStore((s) => s.status);
  const setEnabled = useConnectionStore((s) => s.setWholeLaptop);
  const [support, setSupport] = useState<Support | null>(null);
  const [message, setMessage] = useState("");
  const [restoring, setRestoring] = useState(false);
  const locked = status.state !== "Idle" && status.state !== "Error";

  useEffect(() => {
    let disposed = false;
    invoke<Support>("get_whole_laptop_support").then(
      (result) => { if (!disposed) setSupport(result); },
      (error: unknown) => { if (!disposed) setMessage(String(error)); },
    );
    return () => { disposed = true; };
  }, []);

  async function restore() {
    setRestoring(true);
    setMessage("");
    try {
      await invoke("restore_normal_networking");
      setMessage("Whole-laptop routing is off. Normal Windows networking is restored.");
    } catch (error) {
      setMessage(String(error));
    } finally {
      setRestoring(false);
    }
  }

  return (
    <div className="flex flex-col gap-2 rounded-md bg-black/10 p-2 ring-1 ring-white/10">
      <div className="flex items-center justify-between gap-3">
        <span className="text-xs text-muted-foreground">Route apps through my final proxy</span>
        <Switch
          checked={enabled}
          disabled={locked || restoring || !support?.supported || !support.available || !finalProxy}
          onCheckedChange={setEnabled}
          aria-label="Whole laptop through final proxy"
        />
      </div>
      <p className="text-xs leading-5 text-muted-foreground">
        Apps use your proxy without individual proxy settings or browser extensions.
        {udpEnabled
          ? " Carries internet TCP, UDP and DNS through your final proxy."
          : " Carries internet TCP and DNS. Enable UDP in Final proxy above for games, voice traffic and other UDP apps."}
        {" Ping (ICMP) is not supported by SOCKS5."}
      </p>
      {support && !support.supported ? (
        <p className="text-xs text-muted-foreground">Available on Windows only.</p>
      ) : (
        <>
          {!finalProxy && <p className="text-xs text-muted-foreground">Enable Final proxy above first.</p>}
          {support && !support.available && (
            <p className="text-xs text-muted-foreground">Install the latest Windows build to add this feature.</p>
          )}
          {support && !support.elevated && (
            <p className="text-xs leading-5 text-muted-foreground">
              Requires administrator access. Quit Aether-GUI from its tray menu,
              then right-click the app and choose Run as administrator.
            </p>
          )}
          <p className="text-[11px] leading-4 text-muted-foreground">
            Active only while connected. Disconnecting or reconnecting restores
            normal routing. This is not a kill switch. Turn off other VPN/TUN
            apps before connecting.
          </p>
          <button
            type="button"
            className="self-start rounded px-2 py-1 text-xs text-foreground ring-1 ring-white/20 hover:bg-white/10 focus-visible:outline-2 focus-visible:outline-primary disabled:opacity-50"
            disabled={locked || restoring || !support?.elevated}
            onClick={() => void restore()}
          >
            {restoring ? "Restoring…" : "Restore normal networking"}
          </button>
        </>
      )}
      {message && <p role="status" className="break-words text-xs leading-5 text-muted-foreground">{message}</p>}
    </div>
  );
}
