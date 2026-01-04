# Project Structure

This document outlines the structure of the Art of Rust website.

## Directory Structure

```
art-of-rust/
├── app/                      # Next.js App Router
│   ├── (auth)/              # Authentication routes (login, register)
│   │   ├── login/
│   │   └── register/
│   ├── (dashboard)/         # Dashboard routes (protected)
│   │   └── dashboard/
│   ├── admin/               # Admin panel (admin only)
│   ├── api/                 # API routes
│   │   ├── auth/            # Authentication API
│   │   ├── products/        # Shop API
│   │   └── posts/           # News API
│   ├── forum/               # Forum pages
│   ├── news/                # News/blog pages
│   ├── shop/                # Shop pages
│   ├── layout.tsx           # Root layout
│   ├── page.tsx             # Homepage
│   └── globals.css          # Global styles
│
├── components/              # Reusable UI components
│   ├── layout/              # Layout components (Navbar, Footer)
│   └── ui/                  # Base UI components (Button, Card, Input, etc.)
│
├── lib/                     # Utility libraries
│   ├── auth.ts              # NextAuth configuration
│   ├── prisma.ts            # Prisma client instance
│   └── utils.ts             # Utility functions
│
├── modules/                 # Feature modules
│   └── auth/                # Authentication module
│       └── components/      # Auth-specific components
│
├── prisma/                  # Database
│   ├── schema.prisma        # Database schema
│   └── seed.ts              # Database seed script
│
├── types/                   # TypeScript type definitions
│   ├── index.ts             # Shared types
│   └── next-auth.d.ts      # NextAuth type extensions
│
├── middleware.ts            # Next.js middleware (auth protection)
├── next.config.js          # Next.js configuration
├── tailwind.config.ts      # Tailwind CSS configuration
├── tsconfig.json           # TypeScript configuration
└── package.json            # Dependencies and scripts
```

## Key Features

### Authentication Module
- User registration and login
- OAuth support (Google, Discord)
- Session management
- Protected routes

### Shop Module
- Product catalog
- Shopping cart
- Order management
- Payment integration (Stripe ready)

### Forum Module
- Categories and threads
- Post replies
- User discussions
- Moderation tools

### News Module
- Blog posts
- Categories and tags
- Comments system
- Featured posts

### Admin Module
- Content management
- User management
- Statistics dashboard
- Site settings

## Module Architecture

The website uses a modular architecture where each feature is self-contained:

- **Modules** (`/modules/`): Feature-specific code
- **Components** (`/components/`): Reusable UI components
- **API Routes** (`/app/api/`): Backend endpoints
- **Pages** (`/app/`): Frontend pages

This structure makes it easy to:
- Add new features
- Remove unused modules
- Maintain and test code
- Scale the application

## Database Schema

The database includes models for:
- Users and authentication
- Products and orders
- Forum posts and replies
- News posts and comments
- Notifications

See `prisma/schema.prisma` for the complete schema.

## API Routes

All API routes are in `/app/api/`:
- `/api/auth/*` - Authentication endpoints
- `/api/products` - Shop products
- `/api/posts` - News posts
- More routes can be added as needed

## Styling

- **Tailwind CSS**: Utility-first CSS framework
- **Custom Theme**: Rust-themed color palette
- **Dark Mode**: Built-in dark mode support
- **Responsive**: Mobile-first design

## Environment Variables

Required environment variables (see `.env.example`):
- `DATABASE_URL` - PostgreSQL connection string
- `NEXTAUTH_SECRET` - NextAuth secret key
- `NEXTAUTH_URL` - Application URL
- OAuth credentials (optional)
- Stripe keys (optional)

