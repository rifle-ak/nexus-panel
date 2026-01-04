# Setup Guide - Art of Rust Website

This guide will help you set up the Art of Rust website from scratch.

## Prerequisites

- Node.js 18+ installed
- PostgreSQL database (local or remote)
- npm or yarn package manager

## Step 1: Install Dependencies

```bash
npm install
```

## Step 2: Database Setup

1. Create a PostgreSQL database:
```sql
CREATE DATABASE artofrust;
```

2. Copy the environment file:
```bash
cp .env.example .env
```

3. Update `.env` with your database connection:
```
DATABASE_URL="postgresql://username:password@localhost:5432/artofrust?schema=public"
```

4. Generate Prisma Client:
```bash
npx prisma generate
```

5. Run database migrations:
```bash
npx prisma migrate dev --name init
```

## Step 3: Configure Authentication

1. Generate a NextAuth secret:
```bash
openssl rand -base64 32
```

2. Add it to your `.env`:
```
NEXTAUTH_SECRET="your-generated-secret-here"
NEXTAUTH_URL="http://localhost:3000"
```

3. (Optional) Configure OAuth providers:
   - Google: Get credentials from [Google Cloud Console](https://console.cloud.google.com/)
   - Discord: Get credentials from [Discord Developer Portal](https://discord.com/developers/applications)
   - Add credentials to `.env`

## Step 4: (Optional) Configure Payments

If you want to use Stripe for payments:

1. Get your Stripe keys from [Stripe Dashboard](https://dashboard.stripe.com/)
2. Add to `.env`:
```
STRIPE_SECRET_KEY="sk_..."
STRIPE_PUBLISHABLE_KEY="pk_..."
STRIPE_WEBHOOK_SECRET="whsec_..."
```

## Step 5: Create Admin User

After running migrations, you can create an admin user via Prisma Studio:

```bash
npx prisma studio
```

Or create one programmatically by updating a user's role to `ADMIN` in the database.

## Step 6: Run Development Server

```bash
npm run dev
```

Visit [http://localhost:3000](http://localhost:3000) to see your website!

## Step 7: Seed Initial Data (Optional)

You can create initial forum categories, products, etc. by running:

```bash
npx prisma db seed
```

(You'll need to create a seed script in `prisma/seed.ts`)

## Production Deployment

1. Build the application:
```bash
npm run build
```

2. Start the production server:
```bash
npm start
```

3. Set up environment variables on your hosting platform (Vercel, Railway, etc.)

## Troubleshooting

### Database Connection Issues
- Verify your `DATABASE_URL` is correct
- Ensure PostgreSQL is running
- Check firewall settings if using remote database

### Authentication Issues
- Verify `NEXTAUTH_SECRET` is set
- Check that `NEXTAUTH_URL` matches your domain
- Ensure OAuth credentials are correct if using social login

### Build Errors
- Run `npx prisma generate` after schema changes
- Clear `.next` folder and rebuild: `rm -rf .next && npm run build`

## Next Steps

- Customize the theme colors in `tailwind.config.ts`
- Add more modules as needed
- Configure email service for notifications
- Set up image upload/storage (AWS S3, Cloudinary, etc.)
- Configure CDN for static assets

