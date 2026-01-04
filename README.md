# Art of Rust - Community Website

A full-featured, modular website for the Art of Rust gaming community.

## Features

- 🔐 **Authentication & User Management** - Secure user registration, login, and profile management
- 🛒 **Shop/Marketplace** - Integrated store for game items, cosmetics, and services
- 💬 **Forum/Community** - Discussion boards, threads, and community engagement
- 📰 **News & Blog** - Content management for announcements, updates, and guides
- 👥 **User Profiles** - Customizable user profiles with avatars, stats, and achievements
- 🎮 **Game Integration** - Server status, player stats, and game-related features
- 🎨 **Modern UI** - Beautiful, responsive design with dark mode support
- 🔧 **Admin Dashboard** - Comprehensive admin panel for content and user management
- 📱 **Mobile Responsive** - Fully optimized for all devices

## Tech Stack

- **Framework**: Next.js 14 (App Router)
- **Language**: TypeScript
- **Styling**: Tailwind CSS
- **Database**: PostgreSQL with Prisma ORM
- **Authentication**: NextAuth.js
- **Payments**: Stripe (optional)
- **Real-time**: Socket.io

## Getting Started

### Prerequisites

- Node.js 18+ 
- PostgreSQL database
- npm or yarn

### Installation

1. Clone the repository
2. Install dependencies:
```bash
npm install
```

3. Set up environment variables:
```bash
cp .env.example .env
```

4. Configure your `.env` file with:
   - Database URL
   - NextAuth secret
   - OAuth credentials (if using)
   - Stripe keys (if using payments)

5. Run database migrations:
```bash
npx prisma migrate dev
```

6. Start the development server:
```bash
npm run dev
```

7. Open [http://localhost:3000](http://localhost:3000) in your browser

## Project Structure

```
├── app/                    # Next.js app directory
│   ├── (auth)/            # Authentication routes
│   ├── (dashboard)/       # Dashboard routes
│   ├── api/               # API routes
│   └── layout.tsx         # Root layout
├── components/            # Reusable UI components
├── modules/               # Feature modules
│   ├── auth/              # Authentication module
│   ├── shop/              # Shop/marketplace module
│   ├── forum/             # Forum module
│   ├── news/              # News/blog module
│   ├── users/             # User management module
│   └── admin/             # Admin module
├── lib/                   # Utility functions
├── prisma/                # Database schema
└── types/                 # TypeScript types
```

## Modules

The website is built with a modular architecture, making it easy to add, remove, or customize features:

- **Auth Module**: User authentication and authorization
- **Shop Module**: E-commerce functionality
- **Forum Module**: Community discussions
- **News Module**: Content management
- **Users Module**: User profiles and management
- **Admin Module**: Administrative tools

## Development

- Run type checking: `npm run type-check`
- Run linter: `npm run lint`
- Generate Prisma client: `npx prisma generate`

## License

MIT
