import { LoginForm } from '@/modules/auth/components/login-form'
import Link from 'next/link'

export default function LoginPage() {
  return (
    <div className="container mx-auto px-4 py-16 flex items-center justify-center min-h-[calc(100vh-200px)]">
      <div className="w-full max-w-md">
        <LoginForm />
        <p className="mt-6 text-center text-gray-400">
          Don't have an account?{' '}
          <Link href="/register" className="text-rust-400 hover:text-rust-300">
            Sign up
          </Link>
        </p>
      </div>
    </div>
  )
}

