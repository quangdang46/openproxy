"use client";

import { useState, useEffect } from "react";
import { Card, Button } from "@/shared/components";

export default function ProfilePage() {
  const [user, setUser] = useState(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    fetchUser();
  }, []);

  const fetchUser = async () => {
    try {
      const res = await fetch("/api/user");
      if (res.ok) {
        const data = await res.json();
        setUser(data);
      }
    } catch (error) {
      console.error("Failed to fetch user:", error);
    } finally {
      setLoading(false);
    }
  };

  if (loading) {
    return <div className="text-center py-12">Loading...</div>;
  }

  return (
    <div className="flex flex-col gap-6">
      <h1 className="text-2xl font-bold">Profile</h1>

      <Card padding="lg">
        <div className="flex flex-col gap-4">
          <div>
            <label className="text-sm font-medium text-text-muted">Username</label>
            <p className="text-lg">{user?.username || "N/A"}</p>
          </div>
          <div>
            <label className="text-sm font-medium text-text-muted">Email</label>
            <p className="text-lg">{user?.email || "N/A"}</p>
          </div>
          <div>
            <label className="text-sm font-medium text-text-muted">Role</label>
            <p className="text-lg">{user?.role || "User"}</p>
          </div>
        </div>
      </Card>
    </div>
  );
}
