import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { toast } from "sonner";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "@/components/ui/alert-dialog";
import { getErrorMessage } from "@/lib/errors";
import { invoke } from "@/lib/invoke";
import type { McpApprovalRequest } from "@/types/global";

function riskColor(risk: string): string {
  switch (risk) {
    case "critical":
      return "text-red-600 dark:text-red-400";
    case "high":
      return "text-orange-600 dark:text-orange-400";
    case "medium":
      return "text-amber-600 dark:text-amber-400";
    default:
      return "text-green-600 dark:text-green-400";
  }
}

function riskLabelKey(risk: string): string {
  switch (risk) {
    case "critical":
      return "ai.riskCritical";
    case "high":
      return "ai.riskHigh";
    case "medium":
      return "ai.riskMedium";
    default:
      return "ai.riskLow";
  }
}

/// Global listener + dialog for MCP command approval (Confirm mode).
export function McpApprovalDialog() {
  const { t } = useTranslation();
  const [queue, setQueue] = useState<McpApprovalRequest[]>([]);
  const respondingRef = useRef(false);

  useEffect(() => {
    const unlisten = listen<McpApprovalRequest>("mcp-approval-request", (event) => {
      setQueue((prev) => [...prev, event.payload]);
    });
    return () => {
      unlisten.then((dispose) => dispose());
    };
  }, []);

  const active = queue[0] ?? null;

  const respond = useCallback(
    async (approved: boolean) => {
      if (respondingRef.current || !active) return;
      respondingRef.current = true;
      setQueue((prev) => prev.slice(1));
      try {
        await invoke("respond_mcp_approval", { key: active.key, approved });
      } catch (error) {
        toast.error(getErrorMessage(error));
      } finally {
        respondingRef.current = false;
      }
    },
    [active],
  );

  return (
    <AlertDialog
      open={active !== null}
      onOpenChange={(open) => {
        if (!open) void respond(false);
      }}
    >
      <AlertDialogContent size="sm">
        <AlertDialogHeader>
          <AlertDialogTitle>{t("ai.mcpApprovalTitle")}</AlertDialogTitle>
          <AlertDialogDescription>{t("ai.mcpApprovalDesc")}</AlertDialogDescription>
        </AlertDialogHeader>

        {active ? (
          <div className="space-y-3 text-sm">
            <div>
              <div className="text-xs text-muted-foreground">{t("ai.mcpApprovalSession")}</div>
              <div className="font-mono text-xs">{active.sessionId}</div>
            </div>
            <div>
              <div className="text-xs text-muted-foreground">{t("ai.mcpApprovalCommand")}</div>
              <code className="block rounded bg-muted px-2 py-1.5 font-mono text-xs">
                {active.command}
              </code>
            </div>
            <div className="flex items-center gap-2">
              <span className="text-xs text-muted-foreground">{t("ai.mcpApprovalRisk")}</span>
              <span className={`text-sm font-medium ${riskColor(active.riskLevel)}`}>
                {t(riskLabelKey(active.riskLevel))}
              </span>
            </div>
          </div>
        ) : null}

        <AlertDialogFooter>
          <AlertDialogCancel>{t("common.cancel")}</AlertDialogCancel>
          <AlertDialogAction onClick={() => void respond(true)}>
            {t("common.confirm")}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  );
}
