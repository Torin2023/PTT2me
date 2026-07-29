import type { Metadata } from "next";
import { Geist, Geist_Mono } from "next/font/google";
import { headers } from "next/headers";
import "./globals.css";

const geistSans = Geist({
  variable: "--font-geist-sans",
  subsets: ["latin", "cyrillic"],
});

const geistMono = Geist_Mono({
  variable: "--font-geist-mono",
  subsets: ["latin", "cyrillic"],
});

export async function generateMetadata(): Promise<Metadata> {
  const requestHeaders = await headers();
  const host = requestHeaders.get("host") ?? "localhost";
  const protocol = host.startsWith("localhost") ? "http" : "https";
  const origin = `${protocol}://${host}`;

  return {
    metadataBase: new URL(origin),
    title: "PTT2me — локальная диктовка для macOS",
    description:
      "Назначьте удобную клавишу, говорите по-русски и вставляйте результат в поле, где находится курсор. Полностью локально на Apple Silicon.",
    openGraph: {
      type: "website",
      locale: "ru_RU",
      title: "PTT2me — локальная диктовка для macOS",
      description:
        "Назначьте удобную клавишу, говорите по-русски и вставляйте результат в поле, где находится курсор. Полностью локально на Apple Silicon.",
      images: [
        {
          url: "/og.png",
          width: 1200,
          height: 630,
          alt: "PTT2me — Говорите, текст уже там",
        },
      ],
    },
    twitter: {
      card: "summary_large_image",
      title: "PTT2me — локальная диктовка для macOS",
      description:
        "Назначьте удобную клавишу, говорите по-русски и вставляйте результат в поле, где находится курсор. Полностью локально на Apple Silicon.",
      images: ["/og.png"],
    },
    icons: {
      icon: "/favicon.svg",
      shortcut: "/favicon.svg",
    },
  };
}

export default function RootLayout({
  children,
}: Readonly<{
  children: React.ReactNode;
}>) {
  return (
    <html lang="ru">
      <body
        className={`${geistSans.variable} ${geistMono.variable} antialiased`}
      >
        {children}
      </body>
    </html>
  );
}
