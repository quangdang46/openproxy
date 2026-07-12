"use client";

import { useState, useEffect } from "react";
import { useNotificationStore } from "@/store/notificationStore";
import Sidebar from "../Sidebar";
import Header from "../Header";
import React from "react";
import { initializeApp } from "@/shared/services/initializeApp";

interface DashboardLayoutProps {
  children: React.ReactNode;
}

function getToastStyle(type: string) {
  if (type === "success") {
    return {
      wrapper: "bg-surface border border-hairline before:bg-success-text",
      iconColor: "text-success-text",
      titleColor: "text-ink",
      icon: "check_circle",
    };
  }
  if (type === "error") {
    return {
      wrapper: "bg-surface border border-hairline before:bg-brand-coral",
      iconColor: "text-brand-coral",
      titleColor: "text-ink",
      icon: "error",
    };
  }
  if (type === "warning") {
    return {
      wrapper: "bg-surface border border-hairline before:bg-accent-amber",
      iconColor: "text-accent-amber",
      titleColor: "text-ink",
      icon: "warning",
    };
  }
  return {
    wrapper: "bg-surface border border-hairline before:bg-brand-blue-deep",
    iconColor: "text-brand-blue-deep",
    titleColor: "text-ink",
    icon: "info",
  };
}

export default function DashboardLayout({ children }: DashboardLayoutProps) {
  const [sidebarOpen, setSidebarOpen] = useState(false);
  const [pathname, setPathname] = useState("");
  const [mounted, setMounted] = useState(false);

  useEffect(() => {
    setMounted(true);
    setPathname(window.location.pathname);
    document.body.classList.add("dashboard-ready");
    // Resume tunnel/tailscale/MITM + client auto-ping tick (once per tab).
    initializeApp().catch((e) =>
      console.error("[DashboardLayout] initializeApp failed:", (e as Error).message),
    );
    return () => {
      document.body.classList.remove("dashboard-ready");
    };
  }, []);

  const notifications = useNotificationStore((state) => state.notifications);
  const removeNotification = useNotificationStore((state) => state.removeNotification);

  return (
    <div className="flex h-screen w-full overflow-hidden bg-canvas">
      <div className="fixed top-4 right-4 z-[80] flex w-[min(92vw,380px)] flex-col gap-2">
        {notifications.map((n) => {
          const style = getToastStyle(n.type);
          return (
            <div
              key={n.id}
              className={`relative overflow-hidden rounded-mini-md pl-4 pr-3 py-3 shadow-modal before:absolute before:left-0 before:top-0 before:bottom-0 before:w-[3px] ${style.wrapper}`}
            >
              <div className="flex items-start gap-2.5">
                <span className={`material-symbols-outlined text-[20px] leading-5 ${style.iconColor}`}>{style.icon}</span>
                <div className="min-w-0 flex-1">
                  {n.title ? <p className={`text-[13px] font-semibold mb-0.5 ${style.titleColor}`}>{n.title}</p> : null}
                  <p className="text-[12px] leading-snug text-text-muted whitespace-pre-wrap break-words">{n.message}</p>
                </div>
                {n.dismissible ? (
                  <button
                    type="button"
                    onClick={() => removeNotification(n.id)}
                    className="text-text-muted hover:text-ink transition-colors"
                    aria-label="Dismiss notification"
                  >
                    <span className="material-symbols-outlined text-[16px]">close</span>
                  </button>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>
      {/* Mobile sidebar overlay */}
      {sidebarOpen && (
        <div
          className="fixed inset-0 z-40 bg-black/20 lg:hidden"
          onClick={() => setSidebarOpen(false)}
        />
      )}

      {/* Sidebar - Desktop */}
      <div className="hidden lg:contents">
        <Sidebar />
      </div>

      {/* Sidebar - Mobile */}
      <div
        className={`fixed inset-y-0 left-0 z-50 transform lg:hidden transition-transform duration-300 ease-in-out ${
          sidebarOpen ? "translate-x-0" : "-translate-x-full"
        }`}
      >
        <Sidebar onClose={() => setSidebarOpen(false)} />
      </div>

      {/* Main content */}
      <main className="flex flex-col flex-1 h-full min-w-0 relative transition-colors duration-300 isolate">
        {/* Faint grid background */}
        <div className="landing-grid absolute inset-0 pointer-events-none -z-10" aria-hidden="true" />
        <Header key={pathname} onMenuClick={() => setSidebarOpen(true)} />
        <div className={`flex-1 overflow-y-auto custom-scrollbar ${pathname === "/dashboard/basic-chat" ? "" : "p-6 lg:p-10"} ${pathname === "/dashboard/basic-chat" ? "flex flex-col overflow-hidden" : ""}`}>
          <div className={`${pathname === "/dashboard/basic-chat" ? "flex-1 w-full h-full flex flex-col" : "max-w-7xl mx-auto"}`}>{children}</div>
        </div>
      </main>
    </div>
  );
}
